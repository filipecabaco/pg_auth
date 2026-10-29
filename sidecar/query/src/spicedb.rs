//! SpiceDB as the source of truth, over its HTTP gateway (`--http-enabled`: the gRPC API as
//! JSON): LookupResources for "which ids", CheckBulkPermissions for "these ids", Watch for the
//! change feed.
//!
//! Subjects are `type:id` or `type:id#relation` (a userset such as `group:eng#member`). Only a
//! definite permission counts: a caveated relationship evaluated without its context is
//! conditional, and conditional is denied (fail closed), in lists and checks alike.

use crate::engine::{CallError, Change, Listed, causes, send, send_classified, send_json};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;

/// SpiceDB's `--max-lookup-resources-limit` default: the most one LookupResources call returns.
const LOOKUP_PAGE: usize = 1000;
/// Ids per CheckBulkPermissions call; SpiceDB takes up to 10,000, and groups the items of one
/// subject, type and permission into a single dispatch.
pub const MAX_BULK: usize = 1000;
/// SpiceDB rejects writes of more updates than this (max-relationship-updates-per-call, default).
pub const MAX_WRITE: usize = 1000;

pub struct SpiceDb {
    client: reqwest::Client,
    /// Watch: no total timeout, the stream is meant to stay open (an idle one sends nothing: TCP
    /// keepalive, reqwest's default, finds a peer that is gone)
    watch_client: reqwest::Client,
    base: String,
    consistency: Value,
    /// the revision the sidecar's last relationship write produced: reads are at least as fresh
    written_at: Mutex<Option<Value>>,
    /// the revision of the schema in force (authz.set_rules), written here or by another sidecar
    schema_at: Mutex<Option<String>>,
}

/// Whether SpiceDB can hold `id` as an object id: `[a-zA-Z0-9/_|\-=+]`, up to 1024 bytes.
pub fn holds(_object_type: &str, id: &str) -> bool {
    !id.is_empty() && id.len() <= 1024 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_|-=+".contains(&b))
}

impl SpiceDb {
    /// `consistency` is `minimize_latency` (SpiceDB's default: answers may come from a revision
    /// up to its quantization window old, 5 s by default) or `fully_consistent`.
    pub fn new(url: &str, token: &str, consistency: Option<&str>, timeout: Duration) -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = format!("Bearer {token}").parse().map_err(|_| "invalid SpiceDB token".to_string())?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let client = reqwest::Client::builder().default_headers(headers.clone()).timeout(timeout).build().map_err(|e| e.to_string())?;
        let watch_client =
            reqwest::Client::builder().default_headers(headers).connect_timeout(timeout).build().map_err(|e| e.to_string())?;
        let consistency = match consistency.unwrap_or("minimize_latency") {
            "minimize_latency" => json!({"minimize_latency": true}),
            "fully_consistent" => json!({"fully_consistent": true}),
            other => return Err(format!("SPICEDB_CONSISTENCY {other}: expected minimize_latency or fully_consistent")),
        };
        Ok(Self {
            client,
            watch_client,
            base: url.trim_end_matches('/').to_string(),
            consistency,
            written_at: Mutex::new(None),
            schema_at: Mutex::new(None),
        })
    }

    /// LookupResources, paged by its cursor: up to `limit` ids, or TooMany when there are at
    /// least that many.
    pub async fn list(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        limit: usize,
        context: Option<&Value>,
    ) -> Result<Listed, String> {
        // an optional_limit of 0 would mean no limit
        if limit == 0 {
            return Ok(Listed::TooMany);
        }
        let ids = self.lookup(subject, permission, object_type, Some(limit), self.read_consistency(), context).await?;
        Ok(if ids.len() >= limit { Listed::TooMany } else { Listed::Ids(ids) })
    }

    /// Every object the subject may access, for the permission index, at the latest revision: a
    /// fill must reflect every change the change feed reported before it began.
    pub async fn list_all(&self, subject: &str, permission: &str, object_type: &str) -> Result<Vec<String>, String> {
        self.lookup(subject, permission, object_type, None, json!({"fully_consistent": true}), None).await
    }

    /// LookupResources pages: every page repeats the first call (SpiceDB refuses a cursor whose
    /// call differs in anything but the cursor) and reads its revision, which the cursor carries.
    async fn lookup(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        limit: Option<usize>,
        consistency: Value,
        context: Option<&Value>,
    ) -> Result<Vec<String>, String> {
        let page = limit.map_or(LOOKUP_PAGE, |l| l.min(LOOKUP_PAGE));
        let mut body = json!({
            "consistency": consistency,
            "resource_object_type": object_type,
            "permission": permission,
            "subject": subject_reference(subject)?,
            "optional_limit": page,
        });
        if let Some(c) = context {
            body["context"] = c.clone();
        }
        let mut ids = Vec::new();
        loop {
            let lines = self.stream("/v1/permissions/resources", body.clone()).await?;
            let mut cursor = None;
            for line in &lines {
                let result = result_of(line, "lookup resources")?;
                cursor = field(result, "after_result_cursor").cloned().or(cursor);
                // only a definite permission: a conditional one lacks its caveat's context
                if permissionship(result) == Some(1) {
                    let id = field(result, "resource_object_id")
                        .and_then(Value::as_str)
                        .ok_or("spicedb lookup resources: no resource_object_id")?;
                    ids.push(id.to_string());
                }
            }
            match cursor {
                Some(c) if lines.len() >= page && limit.is_none_or(|l| ids.len() < l) => body["optional_cursor"] = c,
                _ => return Ok(ids),
            }
        }
    }

    /// One CheckBulkPermissions call (up to MAX_BULK ids): an answer per id, in order; errored
    /// or conditional answers are denials.
    pub async fn check(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        ids: &[&String],
        context: Option<&Value>,
    ) -> Result<Vec<bool>, String> {
        let subject = subject_reference(subject)?;
        let items: Vec<Value> = ids
            .iter()
            .map(|id| {
                let mut item =
                    json!({"resource": {"object_type": object_type, "object_id": id}, "permission": permission, "subject": subject});
                if let Some(c) = context {
                    item["context"] = c.clone();
                }
                item
            })
            .collect();
        let v = self.post("/v1/permissions/checkbulk", json!({"consistency": self.read_consistency(), "items": items})).await?;
        let pairs = field(&v, "pairs").and_then(Value::as_array).ok_or("spicedb checkbulk: no pairs field")?;
        Ok((0..ids.len()).map(|i| pairs.get(i).and_then(|p| field(p, "item")).and_then(permissionship) == Some(2)).collect())
    }

    /// Watch relationship and schema changes for as long as the sidecar runs, reporting every
    /// message with any. Every connection resumes from a revision: the last one seen, or one read
    /// before the first. Starting from a new revision (the first time, or after SpiceDB refused the
    /// last, e.g. past its GC window) may skip changes, so it is reported as a change. A failed
    /// connection is retried after `retry`.
    pub async fn follow(&self, retry: Duration, changed: mpsc::UnboundedSender<()>) {
        let mut cursor: Option<Value> = None;
        loop {
            if cursor.is_none() {
                match self.revision().await {
                    Ok(token) => {
                        cursor = Some(token);
                        if changed.send(()).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("pg-auth-sidecar: spicedb watch: {e}");
                        tokio::time::sleep(retry).await;
                        continue;
                    }
                }
            }
            match self.watch(&mut cursor, &changed).await {
                Ok(()) => eprintln!("pg-auth-sidecar: spicedb watch ended; reconnecting"),
                Err(e) => eprintln!("pg-auth-sidecar: spicedb watch: {e}"),
            }
            if changed.is_closed() {
                return;
            }
            tokio::time::sleep(retry).await;
        }
    }

    /// One WriteRelationships call (up to MAX_WRITE changes, no relationship twice): TOUCH writes
    /// whether or not the relationship exists, DELETE whether or not it does.
    /// The configured consistency, or at least as fresh as the sidecar's last relationship write
    /// (a ZedToken), so a grant shows at once.
    fn read_consistency(&self) -> Value {
        match self.written_at.lock().ok().and_then(|w| w.clone()) {
            Some(token) if self.consistency.get("fully_consistent").is_none() => json!({"at_least_as_fresh": token}),
            _ => self.consistency.clone(),
        }
    }

    pub async fn write(&self, changes: &[Change]) -> Result<(), CallError> {
        let updates = changes
            .iter()
            .map(|c| {
                let (object_type, object_id) = c
                    .object
                    .split_once(':')
                    .filter(|(t, i)| !t.is_empty() && !i.is_empty())
                    .ok_or_else(|| format!("object {:?}: expected type:id", c.object))?;
                let mut relationship = json!({
                    "resource": {"object_type": object_type, "object_id": object_id},
                    "relation": c.relation,
                    "subject": subject_reference(&c.subject)?,
                });
                // the schema must allow it (`relation viewer: user with expiration`, `use expiration`)
                if let (true, Some(at)) = (c.write, &c.expires_at) {
                    relationship["optional_expires_at"] = json!(at);
                }
                if let (true, Some((name, context))) = (c.write, &c.condition) {
                    relationship["optional_caveat"] = json!({"caveat_name": name, "context": context});
                }
                Ok(json!({"operation": if c.write { "OPERATION_TOUCH" } else { "OPERATION_DELETE" }, "relationship": relationship}))
            })
            .collect::<Result<Vec<Value>, String>>()
            .map_err(|message| CallError { permanent: true, message })?;
        let request = self.client.post(format!("{}/v1/relationships/write", self.base)).json(&json!({"updates": updates}));
        let text = send_classified(request, "spicedb /v1/relationships/write").await?;
        let written = serde_json::from_str::<Value>(&text).ok().and_then(|v| field(&v, "written_at").cloned());
        if let (Some(token), Ok(mut w)) = (written, self.written_at.lock()) {
            *w = Some(token);
        }
        Ok(())
    }

    /// The current revision, to watch from: the one ReadSchema read at.
    async fn revision(&self) -> Result<Value, String> {
        let v = self.post("/v1/schema/read", json!({})).await?;
        field(&v, "read_at").cloned().ok_or_else(|| "spicedb schema read: no read_at".to_string())
    }

    async fn watch(&self, cursor: &mut Option<Value>, changed: &mpsc::UnboundedSender<()>) -> Result<(), String> {
        let mut body = json!({
            "optional_update_kinds": ["WATCH_KIND_INCLUDE_RELATIONSHIP_UPDATES", "WATCH_KIND_INCLUDE_SCHEMA_UPDATES", "WATCH_KIND_INCLUDE_CHECKPOINTS"],
        });
        if let Some(c) = cursor.as_ref() {
            body["optional_start_cursor"] = c.clone();
        }
        let mut resp = self.watch_client.post(format!("{}/v1/watch", self.base)).json(&body).send().await.map_err(|e| causes(&e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // a revision SpiceDB no longer serves: start over from now, as a missed change
            *cursor = None;
            return Err(format!("{status}: {}", text.chars().take(200).collect::<String>()));
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| causes(&e))? {
            buf.extend_from_slice(&chunk);
            while let Some(end) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(&line).map_err(|e| format!("{e}: {line}"))?;
                let result = result_of(&v, "watch")?;
                let updates = field(result, "updates").and_then(Value::as_array).is_some_and(|u| !u.is_empty());
                let schema = field(result, "schema_updated").and_then(Value::as_bool).unwrap_or(false);
                if (updates || schema) && changed.send(()).is_err() {
                    return Ok(());
                }
                if let Some(t) = field(result, "changes_through") {
                    *cursor = Some(t.clone());
                }
            }
        }
        Ok(())
    }

    /// Replace the schema (the text kept in Postgres): its revision (ZedToken), for the other
    /// sidecars to read at.
    pub async fn write_schema(&self, schema: &str) -> Result<Option<String>, CallError> {
        let request = self.client.post(format!("{}/v1/schema/write", self.base)).json(&json!({"schema": schema}));
        let text = send_classified(request, "spicedb write schema").await?;
        let token = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| field(&v, "written_at").and_then(|w| w.get("token")).and_then(Value::as_str).map(str::to_owned));
        if let Some(t) = &token {
            self.use_schema(t);
        }
        Ok(token)
    }

    /// Read from now on at least as fresh as this schema revision: an older one has not its
    /// definitions (`type not found`). Whether it is new here.
    pub fn use_schema(&self, token: &str) -> bool {
        let Ok(mut at) = self.schema_at.lock() else { return false };
        if at.as_deref() == Some(token) {
            return false;
        }
        *at = Some(token.to_owned());
        if let Ok(mut w) = self.written_at.lock() {
            *w = Some(json!({"token": token}));
        }
        true
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        send_json(self.client.post(format!("{}{path}", self.base)).json(&body), &format!("spicedb {path}")).await
    }

    /// A server-streaming call: the gateway answers with one JSON object per line.
    async fn stream(&self, path: &str, body: Value) -> Result<Vec<Value>, String> {
        let text = send(self.client.post(format!("{}{path}", self.base)).json(&body), &format!("spicedb {path}")).await?;
        text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(|e| format!("spicedb {path}: {e}"))).collect()
    }
}

/// A streamed message's result; an error message fails the call (a stream cut short may be
/// partial).
fn result_of<'a>(v: &'a Value, call: &str) -> Result<&'a Value, String> {
    if let Some(e) = v.get("error") {
        return Err(format!("spicedb {call}: {}", e.get("message").and_then(Value::as_str).unwrap_or("error")));
    }
    v.get("result").ok_or_else(|| format!("spicedb {call}: unexpected message {v}"))
}

/// A field by its proto name or its JSON (lowerCamelCase) name: the gateway may emit either.
fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    v.get(name).or_else(|| {
        let mut camel = String::new();
        let mut upper = false;
        for c in name.chars() {
            match c {
                '_' => upper = true,
                c if upper => {
                    camel.extend(c.to_uppercase());
                    upper = false;
                }
                c => camel.push(c),
            }
        }
        v.get(camel)
    })
}

/// A permissionship as its enum number (names and numbers are both valid JSON for it):
/// lookups: 1 has, 2 conditional; checks: 1 no, 2 has, 3 conditional.
fn permissionship(v: &Value) -> Option<i64> {
    match field(v, "permissionship")? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => match s.as_str() {
            "LOOKUP_PERMISSIONSHIP_HAS_PERMISSION" | "PERMISSIONSHIP_NO_PERMISSION" => Some(1),
            "LOOKUP_PERMISSIONSHIP_CONDITIONAL_PERMISSION" | "PERMISSIONSHIP_HAS_PERMISSION" => Some(2),
            "PERMISSIONSHIP_CONDITIONAL_PERMISSION" => Some(3),
            _ => Some(0),
        },
        _ => None,
    }
}

/// `type:id` or `type:id#relation` as a SubjectReference.
fn subject_reference(subject: &str) -> Result<Value, String> {
    let (object, relation) = match subject.split_once('#') {
        Some((o, r)) => (o, Some(r)),
        None => (subject, None),
    };
    let (object_type, object_id) = object
        .split_once(':')
        .filter(|(t, i)| !t.is_empty() && !i.is_empty())
        .ok_or_else(|| format!("subject {subject:?}: expected type:id"))?;
    let mut v = json!({"object": {"object_type": object_type, "object_id": object_id}});
    if let Some(r) = relation {
        v["optional_relation"] = json!(r);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A stand-in for SpiceDB's gateway: answers each request with `respond(path, body)`, and
    /// records the request bodies.
    async fn fake_spicedb(respond: fn(&str, &Value) -> String) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = vec![0; 64 * 1024];
                    // read the headers and the body (content-length)
                    loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(head_end) = text.find("\r\n\r\n") {
                            let len = text[..head_end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            if buf.len() >= head_end + 4 + len {
                                let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                                let body: Value = serde_json::from_slice(&buf[head_end + 4..head_end + 4 + len]).unwrap_or_default();
                                log.lock().unwrap().push(body.clone());
                                let out = respond(&path, &body);
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                                    out.len()
                                );
                                let _ = stream.write_all(response.as_bytes()).await;
                                return;
                            }
                        }
                    }
                });
            }
        });
        (url, seen)
    }

    fn engine(url: &str) -> SpiceDb {
        SpiceDb::new(url, "key", None, Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn lists_definite_permissions_and_reports_full_pages_as_too_many() {
        // three objects, one of them only conditionally: camelCase names and enum strings
        let (url, seen) = fake_spicedb(|_, _| {
            [
                r#"{"result":{"lookedUpAt":{"token":"t1"},"resourceObjectId":"1","permissionship":"LOOKUP_PERMISSIONSHIP_HAS_PERMISSION","afterResultCursor":{"token":"c1"}}}"#,
                r#"{"result":{"lookedUpAt":{"token":"t1"},"resourceObjectId":"2","permissionship":"LOOKUP_PERMISSIONSHIP_CONDITIONAL_PERMISSION","afterResultCursor":{"token":"c2"}}}"#,
                r#"{"result":{"lookedUpAt":{"token":"t1"},"resourceObjectId":"3","permissionship":"LOOKUP_PERMISSIONSHIP_HAS_PERMISSION","afterResultCursor":{"token":"c3"}}}"#,
            ]
            .join("\n")
        })
        .await;
        let e = engine(&url);
        assert!(matches!(e.list("group:eng#member", "viewer", "doc", 1000, None).await.unwrap(), Listed::Ids(ids) if ids == ["1", "3"]));
        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["subject"], json!({"object": {"object_type": "group", "object_id": "eng"}, "optional_relation": "member"}));
        assert_eq!(body["optional_limit"], json!(1000));
        // a page as long as the limit may have more
        assert!(matches!(e.list("user:anne", "viewer", "doc", 3, None).await.unwrap(), Listed::TooMany));
    }

    #[tokio::test]
    async fn later_pages_repeat_the_first_call_but_for_the_cursor() {
        // a full first page with a conditional result, then a short one
        let (url, seen) = fake_spicedb(|_, body| {
            if body.get("optional_cursor").is_some() {
                r#"{"result":{"resourceObjectId":"9","permissionship":"LOOKUP_PERMISSIONSHIP_HAS_PERMISSION","afterResultCursor":{"token":"c3"}}}"#.to_string()
            } else {
                [
                    r#"{"result":{"lookedUpAt":{"token":"t1"},"resourceObjectId":"1","permissionship":"LOOKUP_PERMISSIONSHIP_HAS_PERMISSION","afterResultCursor":{"token":"c1"}}}"#,
                    r#"{"result":{"lookedUpAt":{"token":"t1"},"resourceObjectId":"2","permissionship":"LOOKUP_PERMISSIONSHIP_CONDITIONAL_PERMISSION","afterResultCursor":{"token":"c2"}}}"#,
                ]
                .join("\n")
            }
        })
        .await;
        // a page of 2 with one definite answer: the next page, then 2 ids, as many as the limit
        assert!(matches!(engine(&url).list("user:anne", "viewer", "doc", 2, None).await.unwrap(), Listed::TooMany));
        let seen = seen.lock().unwrap();
        let mut second = seen[1].clone();
        assert_eq!(second["optional_cursor"], json!({"token": "c2"}));
        second.as_object_mut().unwrap().remove("optional_cursor");
        assert_eq!(second, seen[0], "SpiceDB refuses a cursor with any other argument changed");
    }

    #[tokio::test]
    async fn checks_answer_in_order_and_deny_what_spicedb_cannot_hold() {
        let (url, seen) = fake_spicedb(|_, body| {
            // has, no, conditional, an error: in the request's order
            let n = body["items"].as_array().map_or(0, Vec::len);
            let answers = [
                "{\"item\":{\"permissionship\":\"PERMISSIONSHIP_HAS_PERMISSION\"}}",
                "{\"item\":{\"permissionship\":1}}",
                "{\"item\":{\"permissionship\":3}}",
                "{\"error\":{\"message\":\"x\"}}",
            ];
            format!("{{\"pairs\":[{}]}}", answers[..n].join(","))
        })
        .await;
        let ids: Vec<String> = ["1", "has space", "2", "3", "4"].iter().map(ToString::to_string).collect();
        let key = ("user:anne".to_string(), "doc".to_string(), "viewer".to_string());
        let allowed = crate::engine::Engine::SpiceDb(engine(&url)).check(&key, &ids, &crate::engine::Context::default(), 8).await.unwrap();
        assert_eq!(allowed, [true, false, false, false, false]);
        let items = seen.lock().unwrap()[0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["resource"]["object_id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(items, ["1", "2", "3", "4"], "the id SpiceDB cannot hold is never sent");
    }

    #[test]
    fn subjects_and_ids() {
        assert!(subject_reference("anne").is_err());
        assert!(subject_reference("user:").is_err());
        assert!(
            holds("doc", "0b7c1e3a-9d2f-4c41-8a6e-2b1f0c9d7e55")
                && holds("doc", "a/b|c=d+e_f")
                && !holds("doc", "a b")
                && !holds("doc", "")
        );
    }

    #[tokio::test]
    async fn the_watch_reports_changes_and_starts_over_when_its_revision_is_gone() {
        use wiremock::matchers::{body_partial_json, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // the gateway may name fields either way: readAt here, changes_through below
        Mock::given(path("/v1/schema/read"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"readAt": {"token": "r1"}})))
            .mount(&server)
            .await;
        Mock::given(path("/v1/watch"))
            .and(body_partial_json(json!({"optional_start_cursor": {"token": "r1"}})))
            .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
                "{\"result\":{\"changesThrough\":{\"token\":\"r1\"}}}\n",
                "\n",
                "{\"result\":{\"updates\":[{\"operation\":\"OPERATION_TOUCH\"}],\"changes_through\":{\"token\":\"r2\"}}}\n",
                "{\"result\":{\"schemaUpdated\":true,\"changesThrough\":{\"token\":\"r3\"}}}\n",
            )))
            .mount(&server)
            .await;
        // a revision past the GC window: start over
        Mock::given(path("/v1/watch"))
            .and(body_partial_json(json!({"optional_start_cursor": {"token": "r3"}})))
            .respond_with(ResponseTemplate::new(400).set_body_string("revision has expired"))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let e = engine(&server.uri());
        let follow = tokio::spawn(async move { e.follow(Duration::from_millis(10), tx).await });
        // the start (a missed change, as far as anyone knows), an update, a schema change, then
        // after the expired revision a new start
        for _ in 0..4 {
            tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        }
        follow.abort();
        let reads = server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/v1/schema/read").count();
        assert!(reads >= 2, "read the revision again after it expired ({reads})");
    }

    #[test]
    fn stream_errors_fail_the_call() {
        assert!(result_of(&json!({"error": {"message": "boom"}}), "lookup").err().unwrap().contains("boom"));
        assert!(result_of(&json!({"other": 1}), "lookup").err().unwrap().contains("unexpected message"));
        assert_eq!(field(&json!({"readAt": 1}), "read_at"), Some(&json!(1)));
        assert_eq!(permissionship(&json!({"permissionship": "PERMISSIONSHIP_HAS_PERMISSION"})), Some(2));
    }
}
