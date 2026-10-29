//! The source-of-truth engine behind the sidecar. The extension speaks one protocol to every
//! sidecar and never learns which engine answers: each engine maps the same questions onto its
//! own API (openfga.rs, spicedb.rs).

use crate::cerbos::{Cerbos, Plan};
use crate::openfga::OpenFga;
use crate::spicedb::SpiceDb;
use futures_util::stream::{self, StreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

/// subject, object type, permission
pub type Key = (String, String, String);

#[derive(Clone)]
pub enum Listed {
    Ids(Vec<String>),
    TooMany,
}

pub enum Engine {
    OpenFga(OpenFga),
    SpiceDb(SpiceDb),
    Cerbos(Cerbos),
}

/// A change to one relationship, from a relationship table (outbox.rs): written, or deleted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub write: bool,
    pub object: String,
    pub relation: String,
    pub subject: String,
    /// a write's end (RFC 3339), when the row gives one: SpiceDB expires the relationship then;
    /// either way the outbox delivers its delete when it is due
    pub expires_at: Option<String>,
    /// a write's condition (OpenFGA) or caveat (SpiceDB), and the values the row gives it
    pub condition: Option<(String, Value)>,
}

/// A row's context for a condition or caveat: the request's, with the row's own values (which
/// win, for a name both give).
fn item_context(request: Option<&Value>, row: Option<&Value>) -> Option<Value> {
    match (request.and_then(Value::as_object), row.and_then(Value::as_object)) {
        (None, None) => None,
        (r, w) => {
            let mut context = r.cloned().unwrap_or_default();
            context.extend(w.cloned().unwrap_or_default());
            Some(Value::Object(context))
        }
    }
}

/// `changes` cut into requests, in order: at most `max` each, and no relationship twice in one
/// (the engines refuse a request that names one relationship twice).
pub fn batches(changes: &[Change], max: usize) -> Vec<&[Change]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut seen = std::collections::HashSet::new();
    for (i, c) in changes.iter().enumerate() {
        if i - start == max || !seen.insert((&c.object, &c.relation, &c.subject)) {
            out.push(&changes[start..i]);
            start = i;
            seen.clear();
            seen.insert((&c.object, &c.relation, &c.subject));
        }
    }
    if start < changes.len() {
        out.push(&changes[start..]);
    }
    out
}

/// What a check may carry beyond ids: for an engine that decides on attributes, the principal
/// (its roles and attributes) and each resource's attributes, by id position; for conditions and
/// caveats, the request's context.
#[derive(Default)]
pub struct Context<'a> {
    pub principal: Option<&'a Value>,
    pub attrs: Option<&'a [Value]>,
    pub request: Option<&'a Value>,
}

impl Engine {
    /// The name `ping` answers with.
    pub fn name(&self) -> &'static str {
        match self {
            Engine::OpenFga(_) => "openfga",
            Engine::SpiceDb(_) => "spicedb",
            Engine::Cerbos(_) => "cerbos",
        }
    }

    /// The objects of `object_type` the subject may access, or TooMany when there may be `limit`
    /// or more (the extension then checks rows instead).
    pub async fn list(&self, (subject, object_type, permission): &Key, limit: usize, context: Option<&Value>) -> Result<Listed, String> {
        match self {
            Engine::OpenFga(e) => e.list(subject, permission, object_type, limit, context).await,
            Engine::SpiceDb(e) => e.list(subject, permission, object_type, limit, context).await,
            // no relationships to list: rows are checked
            Engine::Cerbos(_) => Ok(Listed::TooMany),
        }
    }

    /// Every object the subject may access, with no cap, at the engine's latest state, for the
    /// permission index: a fill must reflect every change the feed reported before it began. A
    /// list that may be partial is an error, never a set.
    pub async fn list_all(&self, (subject, object_type, permission): &Key) -> Result<Vec<String>, String> {
        match self {
            Engine::OpenFga(e) => e.list_all(subject, permission, object_type).await,
            Engine::SpiceDb(e) => e.list_all(subject, permission, object_type).await,
            Engine::Cerbos(_) => Err("cerbos cannot list a subject's objects".into()),
        }
    }

    /// One answer per id, in order, from calls of the engine's batch size, `concurrency` at a
    /// time. An id the engine cannot hold has no object there: denied without asking. Anything
    /// the engine does not answer is denied.
    pub async fn check(&self, key: &Key, ids: &[String], context: &Context<'_>, concurrency: usize) -> Result<Vec<bool>, String> {
        let (batch, holds): (usize, fn(&str, &str) -> bool) = match self {
            Engine::OpenFga(_) => (crate::openfga::MAX_BATCH, crate::openfga::holds),
            Engine::SpiceDb(_) => (crate::spicedb::MAX_BULK, crate::spicedb::holds),
            Engine::Cerbos(_) => (crate::cerbos::MAX_RESOURCES, |_, id| !id.is_empty()),
        };
        let asked: Vec<usize> = (0..ids.len()).filter(|&i| holds(&key.1, &ids[i])).collect();
        let calls: Vec<_> = asked.chunks(batch).map(|chunk| self.check_call(key, ids, chunk, context)).collect();
        let answers: Vec<Result<Vec<bool>, String>> = stream::iter(calls).buffered(concurrency.max(1)).collect().await;
        let mut out = vec![false; ids.len()];
        for (chunk, answer) in asked.chunks(batch).zip(answers) {
            for (&i, allowed) in chunk.iter().zip(answer?) {
                out[i] = allowed;
            }
        }
        Ok(out)
    }

    /// One of the engine's check calls, for the ids at these positions.
    async fn check_call(
        &self,
        (subject, object_type, permission): &Key,
        ids: &[String],
        at: &[usize],
        context: &Context<'_>,
    ) -> Result<Vec<bool>, String> {
        let chunk: Vec<&String> = at.iter().map(|&i| &ids[i]).collect();
        // each row's context for conditions and caveats: the request's, and the row's values
        let contexts: Vec<Option<Value>> =
            at.iter().map(|&i| item_context(context.request, context.attrs.and_then(|a| a.get(i)))).collect();
        match self {
            Engine::OpenFga(e) => e.check(subject, permission, object_type, &chunk, &contexts).await,
            Engine::SpiceDb(e) => e.check(subject, permission, object_type, &chunk, &contexts).await,
            Engine::Cerbos(e) => {
                let attrs: Vec<Option<&Value>> = at.iter().map(|&i| context.attrs.and_then(|a| a.get(i))).collect();
                e.check(subject, permission, object_type, &chunk, &attrs, context.principal).await
            }
        }
    }

    /// Apply relationship changes, in order. Writing one that exists, or deleting one that does
    /// not, is not an error: a batch the sidecar delivers again after a failure is harmless.
    pub async fn write(&self, changes: &[Change]) -> Result<(), CallError> {
        let max = match self {
            Engine::OpenFga(_) => crate::openfga::MAX_WRITE,
            Engine::SpiceDb(_) => crate::spicedb::MAX_WRITE,
            Engine::Cerbos(_) => {
                return Err(CallError {
                    permanent: true,
                    message: "cerbos keeps no relationships: relationship tables need OpenFGA or SpiceDB".into(),
                });
            }
        };
        for batch in batches(changes, max) {
            match self {
                Engine::OpenFga(e) => e.write(batch).await?,
                Engine::SpiceDb(e) => e.write(batch).await?,
                Engine::Cerbos(_) => {}
            }
        }
        Ok(())
    }

    /// Make the engine run these rules, kept in Postgres (authz.set_rules): OpenFGA's
    /// model (its DSL, converted to the JSON its API takes; JSON as is), SpiceDB's schema, or
    /// Cerbos's policies. Rules the engine (or the conversion) refuses are a permanent error.
    pub async fn push_rules(&self, rules: &[Rule]) -> Result<Option<String>, CallError> {
        let permanent = |message: String| CallError { permanent: true, message };
        match self {
            Engine::OpenFga(e) => {
                let [rule] = rules else { return Err(permanent("openfga keeps one model".into())) };
                let model = if rule.body.trim_start().starts_with('{') {
                    serde_json::from_str(&rule.body).map_err(|m| permanent(format!("openfga model: {m}")))?
                } else {
                    crate::openfga_dsl::to_json(&rule.body).map_err(|m| permanent(format!("openfga model: {m}")))?
                };
                e.write_model(&model).await.map(Some)
            }
            Engine::SpiceDb(e) => {
                let [rule] = rules else { return Err(permanent("spicedb keeps one schema".into())) };
                e.write_schema(&rule.body).await
            }
            Engine::Cerbos(e) => e.put_policies(rules).await.map(|()| None),
        }
    }

    /// Ask with the engine's rules another sidecar pushed (`push_rules`'s id: OpenFGA's model,
    /// SpiceDB's schema revision): whether they are new to this one.
    pub fn use_rules(&self, id: &str) -> bool {
        match self {
            Engine::OpenFga(e) => e.use_model(id),
            Engine::SpiceDb(e) => e.use_schema(id),
            Engine::Cerbos(_) => false,
        }
    }

    /// Which resources the subject may access, as a condition on their attributes; engines that
    /// decide on relationships have none to give.
    ///
    /// For a table whose rows give conditions and caveats their values (protected with
    /// attributes): SpiceDB answers which objects the subject has, and which it has if a caveat
    /// holds for the row (`limit` or more: check every row); OpenFGA's ListObjects takes no
    /// row's values, and fails on a condition missing them, so every row is checked.
    pub async fn plan(
        &self,
        (subject, object_type, permission): &Key,
        principal: Option<&Value>,
        context: Option<&Value>,
        limit: usize,
    ) -> Result<Plan, String> {
        match self {
            Engine::Cerbos(e) => e.plan(subject, permission, object_type, principal).await,
            Engine::SpiceDb(e) => e.plan(subject, permission, object_type, limit, context).await,
            Engine::OpenFga(_) => Ok(Plan::Rows),
        }
    }

    /// Follow the engine's changes from `start` (RFC 3339, the moment the index began guarding
    /// its sets) for as long as the sidecar runs, sending one message per batch of changes, and
    /// one whenever changes may have been missed (the receiver then treats every set as stale).
    /// `poll` is how often a polled feed is read, and how soon a failed one is retried.
    pub async fn follow(&self, start: String, poll: Duration, changed: mpsc::UnboundedSender<()>) {
        match self {
            Engine::OpenFga(e) => e.follow(start, poll, changed).await,
            Engine::SpiceDb(e) => e.follow(poll, changed).await,
            // no change feed: and no permission index to keep fresh
            Engine::Cerbos(_) => {}
        }
    }
}

/// Send a request: the body on success, else an error naming `what`, the status, and the
/// server's message (or the start of the body when it is not JSON, a proxy's error page say).
pub async fn send(request: reqwest::RequestBuilder, what: &str) -> Result<String, String> {
    send_classified(request, what).await.map_err(|e| e.message)
}

/// Rules kept in Postgres (authz.set_rules): a name (`model`, `resource/expense`), the version
/// to push, and the version the engine runs now, if any (put back when Cerbos cannot compile the
/// new one).
#[derive(Debug, Default)]
pub struct Rule {
    pub name: String,
    pub body: String,
    pub previous: Option<String>,
}

/// A failed call: `permanent` when the engine refused the request for good (a 4xx other than a
/// timeout or a rate limit: an invalid relationship), which sending it again will not change.
#[derive(Debug)]
pub struct CallError {
    pub permanent: bool,
    pub message: String,
}

impl From<String> for CallError {
    fn from(message: String) -> Self {
        CallError { permanent: false, message }
    }
}

/// `send`, telling a refusal from a failure.
pub async fn send_classified(request: reqwest::RequestBuilder, what: &str) -> Result<String, CallError> {
    let failed = |e: reqwest::Error| CallError { permanent: false, message: format!("{what}: {}", causes(&e)) };
    let resp = request.send().await.map_err(failed)?;
    let status = resp.status();
    let text = resp.text().await.map_err(failed)?;
    if status.is_success() {
        return Ok(text);
    }
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("message").or_else(|| v.pointer("/error/message")).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| text.chars().take(200).collect());
    let permanent = status.is_client_error() && !matches!(status.as_u16(), 408 | 429);
    Err(CallError { permanent, message: format!("{what} {status}: {message}") })
}

pub async fn send_json(request: reqwest::RequestBuilder, what: &str) -> Result<Value, String> {
    serde_json::from_str(&send(request, what).await?).map_err(|e| format!("{what}: {e}"))
}

/// An error with its sources: reqwest's own message ("error sending request") hides the cause.
pub fn causes(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cerbos::Cerbos;
    use crate::openfga::OpenFga;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn key(subject: &str, object_type: &str, permission: &str) -> Key {
        (subject.into(), object_type.into(), permission.into())
    }

    fn rule(name: &str, body: &str) -> Rule {
        Rule { name: name.into(), body: body.into(), previous: None }
    }

    fn openfga(server: &MockServer) -> Engine {
        let e = OpenFga::new(
            &server.uri(),
            "s1",
            Some("m1".into()),
            Some("secret".into()),
            Some("MINIMIZE_LATENCY".into()),
            Duration::from_secs(5),
            Duration::from_secs(3),
        )
        .unwrap();
        Engine::OpenFga(e)
    }

    fn cerbos(server: &MockServer) -> Engine {
        Engine::Cerbos(Cerbos::new(&server.uri(), Duration::from_secs(5), None).unwrap())
    }

    #[tokio::test]
    async fn rules_kept_in_postgres_reach_each_engine_in_its_own_form() {
        // OpenFGA: the DSL written as JSON, and the new model asked from then on
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/stores/s1/authorization-models"))
            .and(body_partial_json(json!({"schema_version": "1.1", "type_definitions": [{"type": "user"}]})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"authorization_model_id": "m2"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/list-objects"))
            .and(body_partial_json(json!({"authorization_model_id": "m2"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
            .expect(1)
            .mount(&server)
            .await;
        let e = openfga(&server);
        e.push_rules(&[rule("model", "model\n  schema 1.1\ntype user\n")]).await.unwrap();
        e.list(&key("user:anne", "doc", "viewer"), 10, None).await.unwrap();
        // the model another sidecar pushed is new here once, and asked with from then on
        Mock::given(path("/stores/s1/list-objects"))
            .and(body_partial_json(json!({"authorization_model_id": "m3"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
            .expect(1)
            .mount(&server)
            .await;
        assert!(e.use_rules("m3"));
        assert!(!e.use_rules("m3"));
        e.list(&key("user:anne", "doc", "viewer"), 10, None).await.unwrap();
        // a model in modules is refused for good, without asking OpenFGA
        let refused = e.push_rules(&[rule("model", "model\n  schema 1.1\nmodule docs\n")]).await;
        assert!(refused.as_ref().is_err_and(|e| e.permanent && e.message.contains("modules")), "{:?}", refused.err().map(|e| e.message));

        // SpiceDB: the schema's text
        let server = MockServer::start().await;
        Mock::given(path("/v1/schema/write"))
            .and(body_partial_json(json!({"schema": "definition user {}"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"writtenAt": {"token": "zed-schema"}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/v1/permissions/checkbulk"))
            .and(body_partial_json(json!({"consistency": {"at_least_as_fresh": {"token": "zed-schema"}}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"pairs": []})))
            .expect(1)
            .mount(&server)
            .await;
        let e = Engine::SpiceDb(crate::spicedb::SpiceDb::new(&server.uri(), "key", None, Duration::from_secs(5)).unwrap());
        e.push_rules(&[rule("schema", "definition user {}")]).await.unwrap();
        // reads after it see its definitions: at least as fresh as the schema
        e.check(&key("user:anne", "doc", "viewer"), &["1".into()], &Context::default(), 1).await.unwrap();
        // its revision is the one in force here; another sidecar's next one is new, once
        assert!(!e.use_rules("zed-schema"));
        assert!(e.use_rules("zed-next") && !e.use_rules("zed-next"));

        // Cerbos: each policy's YAML as JSON, through the Admin API with its credentials
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/admin/policy"))
            .and(wiremock::matchers::header("authorization", "Basic Y2VyYm9zOnNlY3JldA=="))
            .and(body_partial_json(json!({"policies": [{"apiVersion": "api.cerbos.dev/v1", "resourcePolicy": {"resource": "doc"}}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": {}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/admin/policies/inspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": {}})))
            .expect(1)
            .mount(&server)
            .await;
        let admin = Some(("cerbos".to_owned(), "secret".to_owned()));
        let e = Engine::Cerbos(Cerbos::new(&server.uri(), Duration::from_secs(5), admin).unwrap());
        let policy = "apiVersion: api.cerbos.dev/v1\nresourcePolicy:\n  resource: doc\n  version: default\n";
        e.push_rules(&[rule("resource/doc", policy)]).await.unwrap();
        // without the Admin API's credentials it cannot store them: a permanent error
        let refused = cerbos(&server).push_rules(&[rule("resource/doc", policy)]).await;
        assert!(refused.as_ref().is_err_and(|e| e.permanent && e.message.contains("Admin API")));
    }

    // Cerbos stores policies it cannot compile and says so only in its log: inspected after, the
    // versions before go back (a policy new to it is disabled) and the push is refused
    #[tokio::test]
    async fn cerbos_policies_that_do_not_compile_are_replaced_by_the_versions_before() {
        let server = MockServer::start().await;
        let condition = |expr: &str| json!({"policies": [{"resourcePolicy": {"rules": [{"condition": {"match": {"expr": expr}}}]}}]});
        for expr in ["request.resource.attr.nope(1)", "request.resource.attr.ok"] {
            Mock::given(method("PUT"))
                .and(path("/admin/policy"))
                .and(body_partial_json(condition(expr)))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": {}})))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/admin/policies/inspect"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({"code": 13, "message": "could not inspect policies"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/admin/policy"))
            .and(query_param("id", "resource.report.vdefault/acme"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"disabledPolicies": 1})))
            .expect(1)
            .mount(&server)
            .await;
        let admin = Some(("cerbos".to_owned(), "secret".to_owned()));
        let e = Engine::Cerbos(Cerbos::new(&server.uri(), Duration::from_secs(5), admin).unwrap());
        let policy = |resource: &str, scope: &str, expr: &str| {
            format!(
                "apiVersion: api.cerbos.dev/v1\nresourcePolicy:\n  resource: {resource}\n  version: default\n  scope: \"{scope}\"\n  rules:\n    \
                 - actions: [view]\n      effect: EFFECT_ALLOW\n      roles: [user]\n      condition:\n        match:\n          expr: {expr}\n"
            )
        };
        let refused = e
            .push_rules(&[
                Rule {
                    name: "resource/doc".into(),
                    body: policy("doc", "", "request.resource.attr.nope(1)"),
                    previous: Some(policy("doc", "", "request.resource.attr.ok")),
                },
                Rule {
                    name: "resource/report.acme".into(),
                    body: policy("report", "acme", "request.resource.attr.nope(1)"),
                    previous: None,
                },
            ])
            .await;
        assert!(
            refused.as_ref().is_err_and(|e| e.permanent && e.message.contains("does not compile resource/doc, resource/report.acme")),
            "{:?}",
            refused.err()
        );
    }

    #[tokio::test]
    async fn openfga_lists_the_subjects_ids_with_the_pinned_model_and_consistency() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/stores/s1/list-objects"))
            .and(body_partial_json(json!({"type": "doc", "relation": "viewer", "user": "user:anne",
                                           "authorization_model_id": "m1", "consistency": "MINIMIZE_LATENCY"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": ["doc:1", "doc:2", "folder:9"]})))
            .expect(1)
            .mount(&server)
            .await;
        let listed = openfga(&server).list(&key("user:anne", "doc", "viewer"), 1000, None).await.unwrap();
        assert!(matches!(listed, Listed::Ids(ids) if ids == ["1", "2"]), "objects of another type are not ids");
        let request = &server.received_requests().await.unwrap()[0];
        assert_eq!(request.headers.get("authorization").unwrap(), "Bearer secret");
    }

    #[tokio::test]
    async fn openfga_reports_a_full_page_as_too_many() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": ["doc:1", "doc:2", "doc:3"]})))
            .mount(&server)
            .await;
        assert!(matches!(openfga(&server).list(&key("user:anne", "doc", "viewer"), 3, None).await.unwrap(), Listed::TooMany));
    }

    #[tokio::test]
    async fn engine_errors_carry_the_call_status_and_message() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"code": "validation_error", "message": "type 'doc' not found"})))
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/batch-check"))
            .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
            .mount(&server)
            .await;
        let e = openfga(&server);
        let err = e.list(&key("user:anne", "doc", "viewer"), 10, None).await.err().unwrap();
        assert_eq!(err, "openfga /list-objects (model m1) 400 Bad Request: type 'doc' not found");
        let err = e.check(&key("user:anne", "doc", "viewer"), &["1".into()], &Context::default(), 1).await.err().unwrap();
        assert!(err.contains("502") && err.contains("<html>bad gateway</html>"), "{err}");
        // nothing listening: the transport's own causes
        let closed = Engine::OpenFga(
            OpenFga::new("http://127.0.0.1:1", "s1", None, None, None, Duration::from_secs(1), Duration::from_secs(1)).unwrap(),
        );
        let err = closed.list(&key("user:anne", "doc", "viewer"), 10, None).await.err().unwrap();
        assert!(err.starts_with("openfga /list-objects: error sending request"), "{err}");
    }

    #[tokio::test]
    async fn openfga_checks_in_batches_of_fifty_in_order() {
        let server = MockServer::start().await;
        // allowed: even ids; a result left out is a denial
        Mock::given(path("/stores/s1/batch-check"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let mut result = serde_json::Map::new();
                for c in body["checks"].as_array().unwrap() {
                    let object = c["tuple_key"]["object"].as_str().unwrap();
                    let id: u32 = object.trim_start_matches("doc:").parse().unwrap();
                    if id != 7 {
                        result.insert(c["correlation_id"].as_str().unwrap().into(), json!({"allowed": id.is_multiple_of(2)}));
                    }
                }
                ResponseTemplate::new(200).set_body_json(json!({"result": result}))
            })
            .expect(3)
            .mount(&server)
            .await;
        // an id OpenFGA cannot hold (whitespace) is denied without asking
        let mut ids: Vec<String> = (0..120).map(|i| i.to_string()).collect();
        ids[5] = "has space".into();
        let answers = openfga(&server).check(&key("user:anne", "doc", "viewer"), &ids, &Context::default(), 2).await.unwrap();
        for (i, allowed) in answers.iter().enumerate() {
            assert_eq!(*allowed, i != 5 && i != 7 && i % 2 == 0, "id {i}");
        }
        let sizes: Vec<usize> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["checks"].as_array().unwrap().len())
            .collect();
        assert_eq!(sizes.iter().sum::<usize>(), 119);
        assert!(sizes.iter().all(|n| *n <= crate::openfga::MAX_BATCH), "{sizes:?}");
    }

    #[tokio::test]
    async fn openfga_lists_everything_for_the_index_or_fails() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/streamed-list-objects"))
            .and(body_partial_json(json!({"user": "user:anne", "consistency": "HIGHER_CONSISTENCY"})))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("{\"result\":{\"object\":\"doc:1\"}}\n\n{\"result\":{\"object\":\"doc:2\"}}\n"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/streamed-list-objects"))
            .and(body_partial_json(json!({"user": "user:bob"})))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("{\"result\":{\"object\":\"doc:1\"}}\n{\"error\":{\"message\":\"deadline\"}}\n"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/streamed-list-objects"))
            .and(body_partial_json(json!({"user": "user:carl"})))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"surprise\":1}\n"))
            .mount(&server)
            .await;
        let e = openfga(&server);
        assert_eq!(e.list_all(&key("user:anne", "doc", "viewer")).await.unwrap(), ["1", "2"]);
        assert!(e.list_all(&key("user:bob", "doc", "viewer")).await.err().unwrap().contains("deadline"), "a reported error is no set");
        assert!(e.list_all(&key("user:carl", "doc", "viewer")).await.err().unwrap().contains("unexpected line"));
    }

    #[tokio::test]
    async fn openfga_follows_the_change_feed_from_its_token() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/changes"))
            .and(query_param("start_time", "2026-10-29T00:00:00Z"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"changes": [{}, {}], "continuation_token": "t1"})))
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/changes"))
            .and(query_param("continuation_token", "t1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"changes": [], "continuation_token": "t1"})))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let e = openfga(&server);
        let follow = tokio::spawn(async move { e.follow("2026-10-29T00:00:00Z".into(), Duration::from_millis(10), tx).await });
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        // later polls continue from the token: the start time is asked for once
        tokio::time::sleep(Duration::from_millis(50)).await;
        follow.abort();
        let starts =
            server.received_requests().await.unwrap().iter().filter(|r| r.url.query().unwrap_or("").contains("start_time")).count();
        assert_eq!(starts, 1);
    }

    #[tokio::test]
    async fn cerbos_plans_translate_and_unknown_kinds_fail() {
        let server = MockServer::start().await;
        let principal = json!({"roles": ["user"], "attr": {"tier": "gold"}});
        for (action, filter) in [
            ("all", json!({"kind": "KIND_ALWAYS_ALLOWED"})),
            ("none", json!({"kind": "KIND_ALWAYS_DENIED"})),
            (
                "mine",
                json!({"kind": "KIND_CONDITIONAL", "condition": {"expression": {"operator": "eq", "operands": [
                {"variable": "request.resource.attr.owner"}, {"value": "anne"}]}}}),
            ),
            ("odd", json!({"kind": "KIND_CONDITIONAL", "condition": {"expression": {"operator": "hasIntersection", "operands": []}}})),
            ("weird", json!({"kind": "KIND_SOMETHING"})),
        ] {
            Mock::given(path("/api/plan/resources"))
                .and(body_partial_json(json!({"action": action, "principal": {"id": "anne", "roles": ["user"], "attr": {"tier": "gold"}},
                                               "resource": {"kind": "note"}})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"filter": filter})))
                .mount(&server)
                .await;
        }
        let e = cerbos(&server);
        let keys = ["all", "none", "mine", "odd", "weird"].map(|action| key("user:anne", "note", action));
        let plan = |i: usize| e.plan(&keys[i], Some(&principal), None, 1000);
        assert_eq!(plan(0).await.unwrap(), Plan::Always);
        assert_eq!(plan(1).await.unwrap(), Plan::Never);
        assert_eq!(plan(2).await.unwrap(), Plan::Condition(json!({"op": "eq", "args": [{"column": "owner"}, {"value": "anne"}]})));
        assert_eq!(plan(3).await.unwrap(), Plan::Rows, "outside the language: checked per row");
        assert!(plan(4).await.err().unwrap().contains("KIND_SOMETHING"));
        // no roles: Cerbos cannot answer, and nothing is asked
        assert!(e.plan(&key("user:anne", "note", "all"), None, None, 1000).await.err().unwrap().contains("no roles"));
    }

    #[tokio::test]
    async fn cerbos_is_asked_in_the_requests_scope_and_policy_version() {
        // Cerbos picks scoped policies by these: without them a scoped deny never applies
        let server = MockServer::start().await;
        let scoped = json!({"scope": "acme", "policyVersion": "v2"});
        Mock::given(path("/api/plan/resources"))
            .and(body_partial_json(json!({"principal": scoped, "resource": {"kind": "expense", "scope": "acme", "policyVersion": "v2"}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"filter": {"kind": "KIND_ALWAYS_DENIED"}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/check/resources"))
            .and(body_partial_json(json!({"principal": scoped,
                "resources": [{"resource": {"kind": "expense", "id": "1", "scope": "acme", "policyVersion": "v2"}}]})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"results": [{"resource": {"id": "1"}, "actions": {"view": "EFFECT_DENY"}}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let principal = json!({"roles": ["user"], "scope": "acme", "policy_version": "v2"});
        let e = cerbos(&server);
        assert_eq!(e.plan(&key("user:anne", "expense", "view"), Some(&principal), None, 1000).await.unwrap(), Plan::Never);
        let context = Context { principal: Some(&principal), attrs: None, request: None };
        assert_eq!(e.check(&key("user:anne", "expense", "view"), &["1".to_owned()], &context, 1).await.unwrap(), vec![false]);
    }

    #[tokio::test]
    async fn cerbos_checks_resources_with_their_attributes_in_batches() {
        let server = MockServer::start().await;
        // allows resources whose owner is anne; answers the ids it was sent, except id 3, which
        // it answers as another resource (a denial)
        Mock::given(path("/api/check/resources"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let results: Vec<Value> = body["resources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        let id = r["resource"]["id"].as_str().unwrap();
                        let effect = if r["resource"]["attr"]["owner"] == "anne" { "EFFECT_ALLOW" } else { "EFFECT_DENY" };
                        json!({"resource": {"id": if id == "3" { "other" } else { id }}, "actions": {"read": effect}})
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({"results": results}))
            })
            .expect(3)
            .mount(&server)
            .await;
        let ids: Vec<String> = (0..120).map(|i| i.to_string()).collect();
        let attrs: Vec<Value> = (0..120).map(|i| json!({"owner": if i % 3 == 0 { "anne" } else { "bob" }})).collect();
        let principal = json!({"roles": ["user"]});
        let context = Context { principal: Some(&principal), attrs: Some(&attrs), request: None };
        let answers = cerbos(&server).check(&key("user:anne", "note", "read"), &ids, &context, 3).await.unwrap();
        for (i, allowed) in answers.iter().enumerate() {
            assert_eq!(*allowed, i % 3 == 0 && i != 3, "id {i}");
        }
    }

    // a table whose rows give caveats their values: SpiceDB's plan decides definite rows in
    // Postgres and checks only conditional ones, each with its row's values beside the request's;
    // OpenFGA checks every row, with the same
    #[tokio::test]
    async fn rows_giving_caveats_values_are_checked_only_where_the_grant_is_conditional() {
        let server = MockServer::start().await;
        let looked_up = [("1", "LOOKUP_PERMISSIONSHIP_HAS_PERMISSION"), ("2", "LOOKUP_PERMISSIONSHIP_CONDITIONAL_PERMISSION")]
            .map(|(id, p)| format!(r#"{{"result":{{"resourceObjectId":"{id}","permissionship":"{p}"}}}}"#))
            .join("\n");
        Mock::given(path("/v1/permissions/resources"))
            .and(body_partial_json(json!({"context": {"region": "eu"}})))
            .respond_with(ResponseTemplate::new(200).set_body_string(looked_up))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(path("/v1/permissions/checkbulk"))
            .and(body_partial_json(json!({"items": [{"resource": {"object_id": "2"}, "context": {"region": "eu", "level": 3}}]})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"pairs": [{"item": {"permissionship": "PERMISSIONSHIP_HAS_PERMISSION"}}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let e = Engine::SpiceDb(crate::spicedb::SpiceDb::new(&server.uri(), "key", None, Duration::from_secs(5)).unwrap());
        let k = key("user:anne", "doc", "viewer");
        let region = json!({"region": "eu"});
        let Ok(Plan::Condition(plan)) = e.plan(&k, None, Some(&region), 1000).await else { panic!("not a condition") };
        assert_eq!(
            plan,
            json!({"op": "or", "args": [
                {"op": "in", "args": [{"id": true}, {"value": ["1"]}]},
                {"op": "and", "args": [{"op": "in", "args": [{"id": true}, {"value": ["2"]}]}, {"op": "check", "args": []}]}]})
        );
        // too many to list: every row is checked
        assert_eq!(e.plan(&k, None, Some(&region), 2).await.unwrap(), Plan::Rows);
        let row = [json!({"level": 3})];
        let asked = Context { attrs: Some(&row), request: Some(&region), ..Context::default() };
        assert_eq!(e.check(&k, &["2".into()], &asked, 1).await.unwrap(), [true]);

        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/batch-check"))
            .and(body_partial_json(json!({"checks": [{"context": {"region": "eu", "level": 3}}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"0": {"allowed": true}}})))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(openfga(&server).check(&k, &["2".into()], &asked, 1).await.unwrap(), [true]);
    }

    #[tokio::test]
    async fn each_engine_refuses_what_it_cannot_answer() {
        let server = MockServer::start().await;
        let fga = openfga(&server);
        let c = cerbos(&server);
        // a row's values reach OpenFGA only in a check: every row is checked
        assert_eq!(fga.plan(&key("user:anne", "doc", "viewer"), None, None, 1000).await.unwrap(), Plan::Rows);
        assert!(matches!(c.list(&key("user:anne", "note", "read"), 10, None).await.unwrap(), Listed::TooMany));
        assert!(c.list_all(&key("user:anne", "note", "read")).await.is_err());
        let (tx, _rx) = mpsc::unbounded_channel();
        // no change feed: returns at once
        tokio::time::timeout(Duration::from_secs(1), c.follow(String::new(), Duration::from_millis(1), tx)).await.unwrap();
        assert_eq!((fga.name(), c.name()), ("openfga", "cerbos"));
        assert!(server.received_requests().await.unwrap().is_empty(), "none of these asks the engine");
    }

    fn change(write: bool, object: &str, subject: &str) -> Change {
        Change { write, object: object.into(), relation: "member".into(), subject: subject.into(), ..Change::default() }
    }

    #[test]
    fn batches_keep_order_and_never_name_a_relationship_twice() {
        let changes = [
            change(true, "org:1", "user:a"),
            change(true, "org:1", "user:b"),
            change(false, "org:1", "user:a"),
            change(true, "org:2", "user:a"),
            change(true, "org:3", "user:a"),
        ];
        let sizes: Vec<usize> = batches(&changes, 10).iter().map(|b| b.len()).collect();
        assert_eq!(sizes, [2, 3], "the delete of a written relationship starts a new request");
        let sizes: Vec<usize> = batches(&changes, 2).iter().map(|b| b.len()).collect();
        assert_eq!(sizes, [2, 2, 1]);
        assert_eq!(batches(&changes, 2).concat(), changes, "nothing reordered or lost");
        assert_eq!(batches(&[], 5).len(), 0);
    }

    #[tokio::test]
    async fn openfga_writes_relationship_changes_idempotently() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/stores/s1/write"))
            .and(body_partial_json(json!({
                "authorization_model_id": "m1",
                "writes": {"on_duplicate": "ignore", "tuple_keys": [{"user": "team:eng#member", "relation": "member", "object": "org:1"}]},
                "deletes": {"on_missing": "ignore", "tuple_keys": [{"user": "user:a", "relation": "member", "object": "org:2"}]}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        openfga(&server).write(&[change(true, "org:1", "team:eng#member"), change(false, "org:2", "user:a")]).await.unwrap();
        // the write request carries no read consistency, which OpenFGA's write does not take
        let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        assert!(body.get("consistency").is_none());
    }

    #[tokio::test]
    async fn spicedb_touches_and_deletes_relationships() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/relationships/write"))
            .and(body_partial_json(json!({"updates": [
                {"operation": "OPERATION_TOUCH", "relationship": {"resource": {"object_type": "org", "object_id": "1"}, "relation": "member",
                    "subject": {"object": {"object_type": "team", "object_id": "eng"}, "optional_relation": "member"}}},
                {"operation": "OPERATION_DELETE", "relationship": {"resource": {"object_type": "org", "object_id": "2"}, "relation": "member",
                    "subject": {"object": {"object_type": "user", "object_id": "a"}}}}
            ]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"writtenAt": {"token": "t"}})))
            .expect(1)
            .mount(&server)
            .await;
        let e = Engine::SpiceDb(crate::spicedb::SpiceDb::new(&server.uri(), "key", None, Duration::from_secs(5)).unwrap());
        e.write(&[change(true, "org:1", "team:eng#member"), change(false, "org:2", "user:a")]).await.unwrap();
        let refused = e.write(&[change(true, "org", "user:a")]).await.err().unwrap();
        assert!(refused.permanent && refused.message.contains("expected type:id"), "{refused:?}");
        // reads are now at least as fresh as that write
        let lookups = server.received_requests().await.unwrap().len();
        assert_eq!(lookups, 1);
    }

    // a write with an end carries it as the relationship's expiration; OpenFGA has none (the
    // outbox deletes it when it is due)
    #[tokio::test]
    async fn spicedb_expires_a_relationship_written_with_an_end() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/relationships/write"))
            .and(body_partial_json(json!({"updates": [{"operation": "OPERATION_TOUCH",
                "relationship": {"relation": "member", "optional_expires_at": "2026-11-01T12:00:00.000000Z"}}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"writtenAt": {"token": "t"}})))
            .expect(1)
            .mount(&server)
            .await;
        let e = Engine::SpiceDb(crate::spicedb::SpiceDb::new(&server.uri(), "key", None, Duration::from_secs(5)).unwrap());
        let ending = Change { expires_at: Some("2026-11-01T12:00:00.000000Z".into()), ..change(true, "org:1", "user:a") };
        e.write(&[ending]).await.unwrap();
    }

    // OpenFGA refuses a list whose condition lacks a parameter: the rows are checked instead (a
    // check denies each such object); without a context a refusal stays an error
    #[tokio::test]
    async fn a_list_refused_for_a_missing_parameter_has_its_rows_checked() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"code": "validation_error", "message": "Internal Server Error"})))
            .mount(&server)
            .await;
        let e = openfga(&server);
        let k = key("user:anne", "doc", "viewer");
        assert!(matches!(e.list(&k, 10, Some(&json!({"current_time": "2026-10-29T12:00:00Z"}))).await, Ok(Listed::TooMany)));
        assert!(e.list(&k, 10, None).await.is_err());
    }

    // the request's context reaches every list and check, and a relationship's condition or caveat
    // is written with the row's values
    #[tokio::test]
    async fn conditions_and_caveats_get_the_requests_context_and_the_rows() {
        let context = json!({"region": "eu", "current_time": "2026-10-29T12:00:00.000000Z"});
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .and(body_partial_json(json!({"context": context})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/batch-check"))
            .and(body_partial_json(json!({"checks": [{"context": context}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/write"))
            .and(body_partial_json(
                json!({"writes": {"tuple_keys": [{"condition": {"name": "in_region", "context": {"allowed": ["eu"]}}}]}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let e = openfga(&server);
        let k = key("user:anne", "doc", "viewer");
        let asked = Context { request: Some(&context), ..Context::default() };
        e.list(&k, 10, Some(&context)).await.unwrap();
        e.check(&k, &["1".into()], &asked, 1).await.unwrap();
        let conditional =
            Change { condition: Some(("in_region".into(), json!({"allowed": ["eu"]}))), ..change(true, "doc:1", "user:anne") };
        e.write(std::slice::from_ref(&conditional)).await.unwrap();

        let server = MockServer::start().await;
        Mock::given(path("/v1/permissions/resources"))
            .and(body_partial_json(json!({"context": context})))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/v1/permissions/checkbulk"))
            .and(body_partial_json(json!({"items": [{"context": context}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"pairs": []})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/v1/relationships/write"))
            .and(body_partial_json(
                json!({"updates": [{"relationship": {"optional_caveat": {"caveat_name": "in_region", "context": {"allowed": ["eu"]}}}}]}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"writtenAt": {"token": "t"}})))
            .expect(1)
            .mount(&server)
            .await;
        let e = Engine::SpiceDb(crate::spicedb::SpiceDb::new(&server.uri(), "key", None, Duration::from_secs(5)).unwrap());
        e.list(&k, 10, Some(&context)).await.unwrap();
        e.check(&k, &["1".into()], &asked, 1).await.unwrap();
        e.write(&[conditional]).await.unwrap();
    }

    #[tokio::test]
    async fn cerbos_keeps_no_relationships() {
        let server = MockServer::start().await;
        let refused = cerbos(&server).write(&[change(true, "org:1", "user:a")]).await.err().unwrap();
        assert!(refused.permanent && refused.message.contains("need OpenFGA or SpiceDB"));
    }

    #[tokio::test]
    async fn writes_tell_a_refusal_from_a_failure() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/write"))
            .and(body_partial_json(json!({"writes": {"tuple_keys": [{"object": "org:bad"}]}})))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"message": "type 'org' has no relation 'member'"})))
            .mount(&server)
            .await;
        Mock::given(path("/stores/s1/write"))
            .and(body_partial_json(json!({"writes": {"tuple_keys": [{"object": "org:busy"}]}})))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({"message": "rate limited"})))
            .mount(&server)
            .await;
        let e = openfga(&server);
        assert!(e.write(&[change(true, "org:bad", "user:a")]).await.err().unwrap().permanent, "a 400 is a refusal");
        assert!(!e.write(&[change(true, "org:busy", "user:a")]).await.err().unwrap().permanent, "a 429 is worth retrying");
        let closed = Engine::OpenFga(
            OpenFga::new("http://127.0.0.1:1", "s1", None, None, None, Duration::from_secs(1), Duration::from_secs(1)).unwrap(),
        );
        assert!(!closed.write(&[change(true, "org:1", "user:a")]).await.err().unwrap().permanent, "unreachable is worth retrying");
    }

    #[tokio::test]
    async fn reads_after_a_write_skip_openfgas_caches() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/write")).respond_with(ResponseTemplate::new(200).set_body_json(json!({}))).mount(&server).await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
            .mount(&server)
            .await;
        let e = openfga(&server);
        let k = key("user:anne", "doc", "viewer");
        e.list(&k, 10, None).await.unwrap();
        e.write(&[change(true, "org:1", "user:a")]).await.unwrap();
        e.list(&k, 10, None).await.unwrap();
        let consistency: Vec<Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("list-objects"))
            .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["consistency"].clone())
            .collect();
        assert_eq!(consistency, [json!("MINIMIZE_LATENCY"), json!("HIGHER_CONSISTENCY")]);
    }

    #[tokio::test]
    async fn reads_after_a_write_are_at_least_as_fresh_in_spicedb() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/relationships/write"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"writtenAt": {"token": "zed1"}})))
            .mount(&server)
            .await;
        Mock::given(path("/v1/permissions/checkbulk"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"pairs": []})))
            .mount(&server)
            .await;
        let e = Engine::SpiceDb(crate::spicedb::SpiceDb::new(&server.uri(), "key", None, Duration::from_secs(5)).unwrap());
        let k = key("user:anne", "doc", "viewer");
        e.check(&k, &["1".into()], &Context::default(), 1).await.unwrap();
        e.write(&[change(true, "org:1", "user:a")]).await.unwrap();
        e.check(&k, &["1".into()], &Context::default(), 1).await.unwrap();
        let consistency: Vec<Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("checkbulk"))
            .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["consistency"].clone())
            .collect();
        assert_eq!(consistency, [json!({"minimize_latency": true}), json!({"at_least_as_fresh": {"token": "zed1"}})]);
    }
}
