//! Wire protocol between the pg_auth extension and the sidecar: one JSON object per line over a
//! Unix socket, one response line per request line.
//!
//! ```text
//! {"op":"list","subject":"user:anne","permission":"viewer","type":"document"}
//!   -> {"ids":["1","2"],"engine_us":840,"valid_ms":1000} | {"too_many":true,"engine_us":1200} | {"error":"..."}
//!      valid_ms: how long callers may keep the answer: until the sidecar's cached answer reaches
//!      half its TTL, when asking again refreshes it in the background (absent when it caches
//!      nothing)
//! {"op":"check","subject":"user:anne","permission":"viewer","type":"document","ids":["1","9"]}
//!   -> {"allowed":[true,false],"engine_us":0} | {"error":"..."}
//! {"op":"ping"}
//!   -> {"engine":"openfga"}
//! {"op":"plan","subject":"user:anne","principal":{"roles":["user"],"attr":{}},"permission":"read","type":"document"}
//!   -> {"plan":"always"} | {"plan":"never"} | {"plan":"rows"} | {"plan":"condition","condition":{...}}
//!      a condition over the resource's attributes (an engine that decides on them, Cerbos):
//!      {"op":"and|or|not|eq|ne|lt|le|gt|ge|in","args":[...]}, an argument being
//!      {"column":"owner_id"}, {"id":true} (the resource's id), {"value":...} or an expression;
//!      "rows": the condition is outside that language, check each row with its attributes
//! {"op":"fill","subject":"user:anne","permission":"viewer","type":"document"}
//!   -> {"queued":true} | {"queued":false}     (permission index; false when it is disabled)
//! {"op":"fill","subject":"user:anne","permission":"viewer","type":"document","wait_ms":10000}
//!   -> {"queued":true,"fresh":true}           (answered once the fill ends or wait_ms passes:
//!                                              whether the set is fresh now)
//! ```
//! Ids are bare object ids (no `type:` prefix). `engine_us` is the time this request spent waiting
//! on the engine (0 when answered from the sidecar's cache); callers may ignore it. `check` may
//! carry `"attrs"`, one object per id, and any request a `"principal"` (its roles and
//! attributes); engines that decide on relationships ignore both.

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Every object of `type` the subject may access with `permission`, or `too_many` when there
    /// are at least the sidecar's LIST_LIMIT (the caller then checks rows instead).
    List {
        subject: String,
        permission: String,
        #[serde(rename = "type")]
        object_type: String,
    },
    /// May the subject access each of these objects (with these attributes)?
    Check {
        subject: String,
        permission: String,
        #[serde(rename = "type")]
        object_type: String,
        ids: Vec<String>,
        attrs: Option<Vec<serde_json::Value>>,
        principal: Option<serde_json::Value>,
    },
    /// Which resources of `type` may the subject access, as a condition on their attributes?
    Plan {
        subject: String,
        permission: String,
        #[serde(rename = "type")]
        object_type: String,
        principal: Option<serde_json::Value>,
    },
    /// Fill the subject's set in the permission index, in the background; with `wait_ms`,
    /// answer once that fill ends (or after `wait_ms`).
    Fill {
        subject: String,
        permission: String,
        #[serde(rename = "type")]
        object_type: String,
        wait_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Response {
    Engine {
        engine: String,
    },
    Ids {
        ids: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        valid_ms: Option<u64>,
    },
    TooMany {
        too_many: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        valid_ms: Option<u64>,
    },
    Allowed {
        allowed: Vec<bool>,
    },
    Plan {
        plan: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        valid_ms: Option<u64>,
    },
    Queued {
        queued: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        fresh: Option<bool>,
    },
    Error {
        error: String,
    },
}

/// Parse one request line; a malformed line is answered with an error, never a crash.
pub fn parse(line: &str) -> Result<Request, String> {
    serde_json::from_str(line).map_err(|e| format!("bad request: {e}"))
}

pub fn encode(response: &Response, engine: Duration) -> String {
    let mut value = serde_json::to_value(response).unwrap_or_else(|_| serde_json::json!({"error": "encode"}));
    if matches!(response, Response::Ids { .. } | Response::TooMany { .. } | Response::Allowed { .. } | Response::Plan { .. }) {
        value["engine_us"] = u64::try_from(engine.as_micros()).unwrap_or(u64::MAX).into();
    }
    let mut s = value.to_string();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests() {
        assert_eq!(
            parse(r#"{"op":"list","subject":"user:anne","permission":"viewer","type":"document"}"#),
            Ok(Request::List { subject: "user:anne".into(), permission: "viewer".into(), object_type: "document".into() })
        );
        assert_eq!(
            parse(r#"{"op":"check","subject":"user:anne","permission":"viewer","type":"document","ids":["1"]}"#),
            Ok(Request::Check {
                subject: "user:anne".into(),
                permission: "viewer".into(),
                object_type: "document".into(),
                ids: vec!["1".into()],
                attrs: None,
                principal: None
            })
        );
        assert_eq!(parse(r#"{"op":"ping"}"#), Ok(Request::Ping));
        assert_eq!(
            parse(r#"{"op":"fill","subject":"user:anne","permission":"viewer","type":"document"}"#),
            Ok(Request::Fill { subject: "user:anne".into(), permission: "viewer".into(), object_type: "document".into(), wait_ms: None })
        );
        assert!(parse(r#"{"op":"drop_tables"}"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn encodes_responses_as_one_line() {
        let ms = Duration::from_millis(2);
        assert_eq!(encode(&Response::Ids { ids: vec!["1".into()], valid_ms: None }, ms), "{\"engine_us\":2000,\"ids\":[\"1\"]}\n");
        assert_eq!(encode(&Response::TooMany { too_many: true, valid_ms: None }, Duration::ZERO), "{\"engine_us\":0,\"too_many\":true}\n");
        assert_eq!(encode(&Response::Allowed { allowed: vec![true, false] }, ms), "{\"allowed\":[true,false],\"engine_us\":2000}\n");
        assert_eq!(encode(&Response::Error { error: "x".into() }, ms), "{\"error\":\"x\"}\n");
    }
}
