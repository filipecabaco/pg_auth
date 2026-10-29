//! OpenFGA as the source of truth: ListObjects for "which ids", BatchCheck for "these ids",
//! StreamedListObjects to fill the permission index, ReadChanges (polled) as the change feed.

use crate::engine::{CallError, Change, Listed, send_classified, send_json};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// OpenFGA rejects batches larger than this (maxChecksPerBatchCheck).
pub const MAX_BATCH: usize = 50;
/// OpenFGA rejects writes of more tuples than this (maxTuplesPerWrite).
pub const MAX_WRITE: usize = 100;
/// After the sidecar writes relationships, reads skip OpenFGA's caches this long (its check cache's
/// default TTL), so a grant or a revocation shows at once.
const FRESH_AFTER_WRITE: Duration = Duration::from_secs(10);

pub struct OpenFga {
    client: reqwest::Client,
    base: String,
    /// the model asked; replaced when the sidecar writes a new one (authz.set_rules); none: the latest
    model_id: Mutex<Option<String>>,
    consistency: Option<String>,
    list_deadline: Duration,
    /// until when reads skip the caches (relationships were just written)
    fresh_until: Mutex<Option<Instant>>,
}

/// Whether OpenFGA can hold `type:id` as an object (`^[^\s]{2,256}$`): any other id is denied
/// without asking, since one would fail the whole BatchCheck.
pub fn holds(object_type: &str, id: &str) -> bool {
    !id.is_empty() && !id.chars().any(char::is_whitespace) && object_type.chars().count() + 1 + id.chars().count() <= 256
}

impl OpenFga {
    /// `list_deadline` is the server's listObjectsDeadline (see `list`).
    pub fn new(
        url: &str,
        store: &str,
        model_id: Option<String>,
        token: Option<String>,
        consistency: Option<String>,
        timeout: Duration,
        list_deadline: Duration,
    ) -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(t) = token {
            let value = format!("Bearer {t}").parse().map_err(|_| "invalid API token".to_string())?;
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        let client = reqwest::Client::builder().default_headers(headers).timeout(timeout).build().map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            base: format!("{}/stores/{store}", url.trim_end_matches('/')),
            model_id: Mutex::new(model_id),
            consistency,
            list_deadline,
            fresh_until: Mutex::new(None),
        })
    }

    /// The model asked: the one pinned or last written, or none (the store's latest).
    fn model(&self) -> Option<String> {
        self.model_id.lock().ok().and_then(|m| m.clone())
    }

    /// Write a model (its JSON, converted from the DSL kept in Postgres) and ask it from now on.
    pub async fn write_model(&self, model: &Value) -> Result<String, CallError> {
        let body =
            send_classified(self.client.post(format!("{}/authorization-models", self.base)).json(model), "openfga write model").await?;
        let id = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("authorization_model_id").and_then(Value::as_str).map(str::to_owned))
            .ok_or_else(|| CallError { permanent: false, message: "openfga write model: no authorization_model_id".into() })?;
        if let Ok(mut m) = self.model_id.lock() {
            *m = Some(id.clone());
        }
        Ok(id)
    }

    /// Ask with this model from now on (one another sidecar wrote): whether it is a new one.
    pub fn use_model(&self, id: &str) -> bool {
        let Ok(mut model) = self.model_id.lock() else { return false };
        if model.as_deref() == Some(id) {
            return false;
        }
        *model = Some(id.to_owned());
        true
    }

    /// Poll the change feed from `start` every `poll`, reading it until it has nothing new each
    /// time, and report every page with changes. An error is retried from the same token, so
    /// nothing is missed.
    pub async fn follow(&self, start: String, poll: Duration, changed: mpsc::UnboundedSender<()>) {
        let mut token: Option<String> = None;
        loop {
            loop {
                match self.changes(token.as_deref(), &start).await {
                    Ok((n, next)) => {
                        if n > 0 && changed.send(()).is_err() {
                            return;
                        }
                        if !next.is_empty() {
                            token = Some(next);
                        }
                        if n == 0 {
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("pg-auth-sidecar: change feed: {e}");
                        break;
                    }
                }
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// A request body with the pinned model, and the configured consistency unless given.
    fn body(&self, mut body: Value) -> Value {
        if let Some(m) = self.model() {
            body["authorization_model_id"] = json!(m);
        }
        let fresh = self.fresh_until.lock().ok().and_then(|f| *f).is_some_and(|until| Instant::now() < until);
        if body.get("consistency").is_none() {
            if fresh {
                body["consistency"] = json!("HIGHER_CONSISTENCY");
            } else if let Some(c) = &self.consistency {
                body["consistency"] = json!(c);
            }
        }
        body
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        let body = self.body(body);
        // an answer about the model (a type it lacks) names the model asked
        let what = match body.get("authorization_model_id").and_then(Value::as_str) {
            Some(m) => format!("openfga {path} (model {m})"),
            None => format!("openfga {path}"),
        };
        send_json(self.client.post(format!("{}{path}", self.base)).json(&body), &what).await
    }

    /// ListObjects truncates silently twice over, so both cases are reported as TooMany (the
    /// extension then checks rows, which is slower but right):
    /// - at the server's limit (listObjectsMaxResults, 1000 by default): a full page may have more;
    /// - at the server's deadline (listObjectsDeadline, 3 s by default) it answers with the objects
    ///   found so far, as a success. Its clock starts after ours, so an answer that took at least
    ///   the deadline here may be partial.
    pub async fn list(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        limit: usize,
        context: Option<&Value>,
    ) -> Result<Listed, String> {
        let started = Instant::now();
        let mut body = json!({"type": object_type, "relation": permission, "user": subject});
        if let Some(c) = context {
            body["context"] = c.clone();
        }
        let v = match self.post("/list-objects", body).await {
            // ListObjects fails as a whole when a condition lacks a parameter the request did not
            // give (a token without the claim); BatchCheck denies each such object: check rows
            Err(e) if context.is_some() && e.contains(" 400 ") => return Ok(Listed::TooMany),
            answer => answer?,
        };
        let objects = v.get("objects").and_then(Value::as_array).ok_or("openfga list-objects: no objects field")?;
        if objects.len() >= limit || started.elapsed() >= self.list_deadline {
            return Ok(Listed::TooMany);
        }
        let prefix = format!("{object_type}:");
        Ok(Listed::Ids(objects.iter().filter_map(Value::as_str).filter_map(|o| o.strip_prefix(&prefix).map(str::to_owned)).collect()))
    }

    /// Every object of `object_type` the subject may access, with no result cap
    /// (StreamedListObjects), for the permission index, past OpenFGA's caches
    /// (HIGHER_CONSISTENCY): a fill right after a change must not read the answers from before
    /// it. An answer that reports an error or took the deadline may be partial: it is an error,
    /// never a set.
    pub async fn list_all(&self, subject: &str, permission: &str, object_type: &str) -> Result<Vec<String>, String> {
        let body = self.body(json!({"type": object_type, "relation": permission, "user": subject, "consistency": "HIGHER_CONSISTENCY"}));
        let started = Instant::now();
        let request = self.client.post(format!("{}/streamed-list-objects", self.base)).timeout(self.list_deadline * 2).json(&body);
        let text = crate::engine::send(request, "openfga streamed-list-objects").await?;
        if started.elapsed() >= self.list_deadline {
            return Err("openfga streamed-list-objects took the deadline: the list may be partial".into());
        }
        let prefix = format!("{object_type}:");
        let mut ids = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).map_err(|e| format!("openfga streamed-list-objects: {e}"))?;
            if let Some(e) = v.get("error") {
                return Err(format!("openfga streamed-list-objects: {e}"));
            }
            match v.pointer("/result/object").and_then(Value::as_str) {
                Some(o) => ids.extend(o.strip_prefix(&prefix).map(str::to_owned)),
                None => return Err(format!("openfga streamed-list-objects: unexpected line {line}")),
            }
        }
        Ok(ids)
    }

    /// The next page of the store's change feed (ReadChanges): how many changes, and the token
    /// to continue from (unchanged when there is nothing new). Without a token, starts at
    /// `start_time` (RFC 3339).
    async fn changes(&self, token: Option<&str>, start_time: &str) -> Result<(usize, String), String> {
        let mut query = vec![("page_size", "100".to_string())];
        match token {
            Some(t) => query.push(("continuation_token", t.to_string())),
            None => query.push(("start_time", start_time.to_string())),
        }
        let v = send_json(self.client.get(format!("{}/changes", self.base)).query(&query), "openfga changes").await?;
        let n = v.get("changes").and_then(Value::as_array).map_or(0, Vec::len);
        let next = v.get("continuation_token").and_then(Value::as_str).unwrap_or_default().to_string();
        Ok((n, next))
    }

    /// One Write call (up to MAX_WRITE changes, no tuple twice): writes and deletes together,
    /// a tuple written that exists or deleted that does not ignored.
    pub async fn write(&self, changes: &[Change]) -> Result<(), CallError> {
        let key = |c: &Change| json!({"user": c.subject, "relation": c.relation, "object": c.object});
        let written = |c: &Change| {
            let mut k = key(c);
            if let Some((name, context)) = &c.condition {
                k["condition"] = json!({"name": name, "context": context});
            }
            k
        };
        let writes: Vec<Value> = changes.iter().filter(|c| c.write).map(written).collect();
        let deletes: Vec<Value> = changes.iter().filter(|c| !c.write).map(key).collect();
        let mut body = json!({});
        if !writes.is_empty() {
            body["writes"] = json!({"tuple_keys": writes, "on_duplicate": "ignore"});
        }
        if !deletes.is_empty() {
            body["deletes"] = json!({"tuple_keys": deletes, "on_missing": "ignore"});
        }
        if let Some(m) = self.model() {
            body["authorization_model_id"] = json!(m);
        }
        send_classified(self.client.post(format!("{}/write", self.base)).json(&body), "openfga write").await?;
        if let Ok(mut f) = self.fresh_until.lock() {
            *f = Some(Instant::now() + FRESH_AFTER_WRITE);
        }
        Ok(())
    }

    /// One BatchCheck call (up to MAX_BATCH ids): an answer per id, in order; missing or errored
    /// results are denials.
    pub async fn check(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        ids: &[&String],
        contexts: &[Option<Value>],
    ) -> Result<Vec<bool>, String> {
        let checks: Vec<Value> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut check = json!({
                    "tuple_key": {"user": subject, "relation": permission, "object": format!("{object_type}:{id}")},
                    "correlation_id": i.to_string()
                });
                if let Some(Some(c)) = contexts.get(i) {
                    check["context"] = c.clone();
                }
                check
            })
            .collect();
        let v = self.post("/batch-check", json!({ "checks": checks })).await?;
        let result = v.get("result").ok_or("openfga batch-check: no result field")?;
        Ok((0..ids.len())
            .map(|i| result.get(i.to_string()).and_then(|r| r.get("allowed")).and_then(Value::as_bool).unwrap_or(false))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A stand-in for OpenFGA's list-objects that answers `["doc:1"]` after `delay`, like the real
    /// server does when its deadline cuts a ListObjects short.
    async fn fake_openfga(delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0; 64 * 1024];
                    let _ = stream.read(&mut buf).await;
                    tokio::time::sleep(delay).await;
                    let body = r#"{"objects":["doc:1"]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        url
    }

    async fn list(delay: Duration, deadline: Duration, limit: usize) -> Listed {
        let url = fake_openfga(delay).await;
        let engine = OpenFga::new(&url, "s", None, None, None, Duration::from_secs(5), deadline).unwrap();
        engine.list("user:anne", "viewer", "doc", limit, None).await.unwrap()
    }

    #[tokio::test]
    async fn quick_short_lists_are_complete() {
        assert!(matches!(list(Duration::ZERO, Duration::from_millis(300), 1000).await, Listed::Ids(ids) if ids == ["1"]));
    }

    #[tokio::test]
    async fn full_pages_and_answers_at_the_deadline_may_be_partial() {
        assert!(matches!(list(Duration::ZERO, Duration::from_millis(300), 1).await, Listed::TooMany), "full page");
        assert!(matches!(list(Duration::from_millis(350), Duration::from_millis(300), 1000).await, Listed::TooMany), "deadline");
    }
}
