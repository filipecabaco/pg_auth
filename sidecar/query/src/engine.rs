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

/// What a check may carry beyond ids, for an engine that decides on attributes: the principal
/// (its roles and attributes) and each resource's attributes, by id position.
#[derive(Default)]
pub struct Context<'a> {
    pub principal: Option<&'a Value>,
    pub attrs: Option<&'a [Value]>,
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
    pub async fn list(&self, (subject, object_type, permission): &Key, limit: usize) -> Result<Listed, String> {
        match self {
            Engine::OpenFga(e) => e.list(subject, permission, object_type, limit).await,
            Engine::SpiceDb(e) => e.list(subject, permission, object_type, limit).await,
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
        match self {
            Engine::OpenFga(e) => e.check(subject, permission, object_type, &chunk).await,
            Engine::SpiceDb(e) => e.check(subject, permission, object_type, &chunk).await,
            Engine::Cerbos(e) => {
                let attrs: Vec<Option<&Value>> = at.iter().map(|&i| context.attrs.and_then(|a| a.get(i))).collect();
                e.check(subject, permission, object_type, &chunk, &attrs, context.principal).await
            }
        }
    }

    /// Which resources the subject may access, as a condition on their attributes; engines that
    /// decide on relationships have none to give.
    pub async fn plan(&self, (subject, object_type, permission): &Key, principal: Option<&Value>) -> Result<Plan, String> {
        match self {
            Engine::Cerbos(e) => e.plan(subject, permission, object_type, principal).await,
            _ => Err(format!("{} plans no conditions: protect the table by its id", self.name())),
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
    let resp = request.send().await.map_err(|e| format!("{what}: {}", causes(&e)))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("{what}: {}", causes(&e)))?;
    if status.is_success() {
        return Ok(text);
    }
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("message").or_else(|| v.pointer("/error/message")).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(format!("{what} {status}: {message}"))
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
        Engine::Cerbos(Cerbos::new(&server.uri(), Duration::from_secs(5)).unwrap())
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
        let listed = openfga(&server).list(&key("user:anne", "doc", "viewer"), 1000).await.unwrap();
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
        assert!(matches!(openfga(&server).list(&key("user:anne", "doc", "viewer"), 3).await.unwrap(), Listed::TooMany));
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
        let err = e.list(&key("user:anne", "doc", "viewer"), 10).await.err().unwrap();
        assert_eq!(err, "openfga /list-objects 400 Bad Request: type 'doc' not found");
        let err = e.check(&key("user:anne", "doc", "viewer"), &["1".into()], &Context::default(), 1).await.err().unwrap();
        assert!(err.contains("502") && err.contains("<html>bad gateway</html>"), "{err}");
        // nothing listening: the transport's own causes
        let closed = Engine::OpenFga(
            OpenFga::new("http://127.0.0.1:1", "s1", None, None, None, Duration::from_secs(1), Duration::from_secs(1)).unwrap(),
        );
        let err = closed.list(&key("user:anne", "doc", "viewer"), 10).await.err().unwrap();
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
        let plan = |i: usize| e.plan(&keys[i], Some(&principal));
        assert_eq!(plan(0).await.unwrap(), Plan::Always);
        assert_eq!(plan(1).await.unwrap(), Plan::Never);
        assert_eq!(plan(2).await.unwrap(), Plan::Condition(json!({"op": "eq", "args": [{"column": "owner"}, {"value": "anne"}]})));
        assert_eq!(plan(3).await.unwrap(), Plan::Rows, "outside the language: checked per row");
        assert!(plan(4).await.err().unwrap().contains("KIND_SOMETHING"));
        // no roles: Cerbos cannot answer, and nothing is asked
        assert!(e.plan(&key("user:anne", "note", "all"), None).await.err().unwrap().contains("no roles"));
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
        let context = Context { principal: Some(&principal), attrs: Some(&attrs) };
        let answers = cerbos(&server).check(&key("user:anne", "note", "read"), &ids, &context, 3).await.unwrap();
        for (i, allowed) in answers.iter().enumerate() {
            assert_eq!(*allowed, i % 3 == 0 && i != 3, "id {i}");
        }
    }

    #[tokio::test]
    async fn each_engine_refuses_what_it_cannot_answer() {
        let server = MockServer::start().await;
        let fga = openfga(&server);
        let c = cerbos(&server);
        assert!(fga.plan(&key("user:anne", "doc", "viewer"), None).await.err().unwrap().contains("openfga plans no conditions"));
        assert!(matches!(c.list(&key("user:anne", "note", "read"), 10).await.unwrap(), Listed::TooMany));
        assert!(c.list_all(&key("user:anne", "note", "read")).await.is_err());
        let (tx, _rx) = mpsc::unbounded_channel();
        // no change feed: returns at once
        tokio::time::timeout(Duration::from_secs(1), c.follow(String::new(), Duration::from_millis(1), tx)).await.unwrap();
        assert_eq!((fga.name(), c.name()), ("openfga", "cerbos"));
        assert!(server.received_requests().await.unwrap().is_empty(), "none of these asks the engine");
    }
}
