//! Cerbos as the source of truth: an engine that decides on a resource's attributes, not on
//! relationships. PlanResources answers which resources a principal may access as a condition on
//! their attributes (translated into the protocol's expression language, protocol.rs);
//! CheckResources answers for resources given with their attributes. Cerbos keeps no data: it
//! cannot list a subject's objects, fill the permission index, or report changes.

use crate::engine::send_json;
use serde_json::{Map, Value, json};
use std::time::Duration;

/// Cerbos's `maxResourcesPerRequest` default: the most one CheckResources call takes.
pub const MAX_RESOURCES: usize = 50;

pub struct Cerbos {
    client: reqwest::Client,
    base: String,
}

/// What a plan says about a resource kind for a principal (protocol.rs, `plan`).
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Always,
    Never,
    /// a condition outside the protocol's language: check each resource with its attributes
    Rows,
    Condition(Value),
}

impl Cerbos {
    pub fn new(url: &str, timeout: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())?;
        Ok(Self { client, base: url.trim_end_matches('/').to_string() })
    }

    pub async fn plan(&self, subject: &str, permission: &str, object_type: &str, principal: Option<&Value>) -> Result<Plan, String> {
        let body = json!({
            "requestId": "pg_auth",
            "action": permission,
            "principal": principal_of(subject, principal)?,
            "resource": {"kind": object_type},
        });
        let v = send_json(self.client.post(format!("{}/api/plan/resources", self.base)).json(&body), "cerbos plan resources").await?;
        let filter = v.get("filter").ok_or("cerbos plan resources: no filter")?;
        Ok(match filter.get("kind").and_then(Value::as_str) {
            Some("KIND_ALWAYS_ALLOWED") => Plan::Always,
            Some("KIND_ALWAYS_DENIED") => Plan::Never,
            Some("KIND_CONDITIONAL") => match filter.get("condition").and_then(condition) {
                Some(c) => Plan::Condition(c),
                None => Plan::Rows,
            },
            other => return Err(format!("cerbos plan resources: filter kind {other:?}")),
        })
    }

    /// One CheckResources call (up to MAX_RESOURCES): an answer per id, in order, each resource
    /// with its attributes when given. Anything but an explicit allow is a denial.
    pub async fn check(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        ids: &[&String],
        attrs: &[Option<&Value>],
        principal: Option<&Value>,
    ) -> Result<Vec<bool>, String> {
        let resources: Vec<Value> = ids
            .iter()
            .zip(attrs)
            .map(|(id, attr)| {
                json!({"actions": [permission], "resource": {"kind": object_type, "id": id, "attr": attr.cloned().unwrap_or_else(|| json!({}))}})
            })
            .collect();
        let body = json!({"requestId": "pg_auth", "principal": principal_of(subject, principal)?, "resources": resources});
        let v = send_json(self.client.post(format!("{}/api/check/resources", self.base)).json(&body), "cerbos check resources").await?;
        let results = v.get("results").and_then(Value::as_array).ok_or("cerbos check resources: no results")?;
        // results come in the request's order; one for another resource is a denial
        Ok(ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                results.get(i).is_some_and(|r| {
                    r.pointer("/resource/id").and_then(Value::as_str) == Some(id.as_str())
                        && r.get("actions").and_then(|a| a.get(permission)).and_then(Value::as_str) == Some("EFFECT_ALLOW")
                })
            })
            .collect())
    }
}

/// The principal for a subject `type:id`: its id, and the roles and attributes the request
/// carries. Cerbos needs at least one role: without, the request fails (fail closed).
fn principal_of(subject: &str, principal: Option<&Value>) -> Result<Value, String> {
    let id = subject.split_once(':').map_or(subject, |(_, id)| id);
    let roles = principal.and_then(|p| p.get("roles")).and_then(Value::as_array).filter(|r| !r.is_empty());
    let Some(roles) = roles else {
        return Err(format!(
            "cerbos: no roles for {subject}: Cerbos decides on roles (the token's roles_claim, pg_auth.subject_roles, or authz.why's subject_roles)"
        ));
    };
    let attr = principal.and_then(|p| p.get("attr")).filter(|a| a.is_object()).cloned().unwrap_or_else(|| json!({}));
    Ok(json!({"id": id, "roles": roles, "attr": attr}))
}

/// A Cerbos plan operand in the protocol's expression language, or None when it is outside it.
fn condition(operand: &Value) -> Option<Value> {
    if let Some(v) = operand.get("value") {
        return Some(json!({"value": v}));
    }
    if let Some(var) = operand.get("variable").and_then(Value::as_str) {
        return match var {
            "request.resource.id" => Some(json!({"id": true})),
            _ => var.strip_prefix("request.resource.attr.").filter(|c| !c.contains('.')).map(|c| json!({"column": c})),
        };
    }
    let e = operand.get("expression")?;
    let op = e.get("operator").and_then(Value::as_str)?;
    let args = e.get("operands").and_then(Value::as_array)?;
    match op {
        "and" | "or" | "not" | "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "in" => {
            let args = args.iter().map(condition).collect::<Option<Vec<_>>>()?;
            let arity_ok = match op {
                "not" => args.len() == 1,
                "and" | "or" => !args.is_empty(),
                _ => args.len() == 2,
            };
            arity_ok.then(|| Value::Object(Map::from_iter([("op".into(), json!(op)), ("args".into(), Value::Array(args))])))
        }
        // a list of values, as `in` takes it
        "list" => args.iter().map(|a| a.get("value").cloned()).collect::<Option<Vec<_>>>().map(|vs| json!({"value": vs})),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_translate_into_the_protocols_language_or_rows() {
        // (owner == "anne" or status == "published") and not (archived == true)
        let cerbos = json!({"expression": {"operator": "and", "operands": [
            {"expression": {"operator": "or", "operands": [
                {"expression": {"operator": "eq", "operands": [{"variable": "request.resource.attr.owner"}, {"value": "anne"}]}},
                {"expression": {"operator": "eq", "operands": [{"variable": "request.resource.attr.status"}, {"value": "published"}]}}]}},
            {"expression": {"operator": "not", "operands": [
                {"expression": {"operator": "eq", "operands": [{"variable": "request.resource.attr.archived"}, {"value": true}]}}]}}]}});
        assert_eq!(
            condition(&cerbos),
            Some(json!({"op": "and", "args": [
                {"op": "or", "args": [
                    {"op": "eq", "args": [{"column": "owner"}, {"value": "anne"}]},
                    {"op": "eq", "args": [{"column": "status"}, {"value": "published"}]}]},
                {"op": "not", "args": [{"op": "eq", "args": [{"column": "archived"}, {"value": true}]}]}]}))
        );
        let in_list = json!({"expression": {"operator": "in", "operands": [{"variable": "request.resource.id"}, {"expression": {"operator": "list", "operands": [{"value": "1"}, {"value": "2"}]}}]}});
        assert_eq!(condition(&in_list), Some(json!({"op": "in", "args": [{"id": true}, {"value": ["1", "2"]}]})));
        // arithmetic, nested attributes and anything else are checked row by row
        let add = json!({"expression": {"operator": "gt", "operands": [{"expression": {"operator": "add", "operands": [{"variable": "request.resource.attr.a"}, {"value": 1}]}}, {"value": 2}]}});
        assert_eq!(condition(&add), None);
        assert_eq!(condition(&json!({"variable": "request.resource.attr.meta.owner"})), None);
    }

    #[test]
    fn principals_need_roles() {
        assert!(principal_of("user:anne", None).is_err());
        assert!(principal_of("user:anne", Some(&json!({"roles": []}))).is_err());
        assert_eq!(
            principal_of("user:anne", Some(&json!({"roles": ["user"]}))).unwrap(),
            json!({"id": "anne", "roles": ["user"], "attr": {}})
        );
    }
}
