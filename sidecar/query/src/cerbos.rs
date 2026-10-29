//! Cerbos as the source of truth: an engine that decides on a resource's attributes, not on
//! relationships. PlanResources answers which resources a principal may access as a condition on
//! their attributes (translated into the protocol's expression language, protocol.rs);
//! CheckResources answers for resources given with their attributes. Cerbos keeps no data: it
//! cannot list a subject's objects, fill the permission index, or report changes.

use crate::engine::{CallError, Rule, send_classified, send_json};
use serde_json::{Map, Value, json};
use std::time::Duration;

/// Cerbos's `maxResourcesPerRequest` default: the most one CheckResources call takes.
pub const MAX_RESOURCES: usize = 50;

pub struct Cerbos {
    client: reqwest::Client,
    base: String,
    /// the Admin API's user and password, to store policies kept in Postgres (authz.set_rules)
    admin: Option<(String, String)>,
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
    pub fn new(url: &str, timeout: Duration, admin: Option<(String, String)>) -> Result<Self, String> {
        let client = reqwest::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())?;
        Ok(Self { client, base: url.trim_end_matches('/').to_string(), admin })
    }

    /// Store policies (each a YAML document, as Cerbos's files are, with the version it replaces)
    /// through the Admin API, which needs a mutable policy store (a database, not a directory) and
    /// the admin credentials. Cerbos stores a policy that does not compile and goes on with the
    /// last policies that did, saying so only in its log: so the policies are inspected after, and
    /// when they do not compile the versions before are stored again (a new policy is disabled),
    /// and the push is refused.
    pub async fn put_policies(&self, policies: &[Rule]) -> Result<(), CallError> {
        let permanent = |message: String| CallError { permanent: true, message };
        let (user, password) = self
            .admin
            .as_ref()
            .ok_or_else(|| permanent("cerbos: storing policies needs its Admin API (CERBOS_ADMIN_USER, CERBOS_ADMIN_PASSWORD)".into()))?;
        let yaml = |p: &str| serde_norway::from_str::<Value>(p).map_err(|e| permanent(format!("cerbos: a policy is not YAML: {e}")));
        let new = policies.iter().map(|p| yaml(&p.body)).collect::<Result<Vec<_>, _>>()?;
        self.put(&new, user, password).await?;
        if self.compiles(user, password).await? {
            return Ok(());
        }
        let mut before = Vec::new();
        let mut disable = Vec::new();
        for (rule, policy) in policies.iter().zip(&new) {
            match &rule.previous {
                Some(p) => before.push(yaml(p)?),
                None => disable.extend(policy_id(policy)),
            }
        }
        if !before.is_empty() {
            self.put(&before, user, password).await?;
        }
        if !disable.is_empty() {
            let ids: Vec<_> = disable.iter().map(|id| ("id", id.as_str())).collect();
            let request = self.client.delete(format!("{}/admin/policy", self.base)).basic_auth(user, Some(password)).query(&ids);
            send_classified(request, "cerbos disable policy").await?;
        }
        let names = policies.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
        Err(permanent(format!("cerbos does not compile {names} (its log says why); the versions before are back in force")))
    }

    async fn put(&self, policies: &[Value], user: &str, password: &str) -> Result<(), CallError> {
        let request =
            self.client.put(format!("{}/admin/policy", self.base)).basic_auth(user, Some(password)).json(&json!({"policies": policies}));
        send_classified(request, "cerbos add or update policy").await.map(|_| ())
    }

    /// Whether the stored policies compile: InspectPolicies fails with INTERNAL (13) when not.
    async fn compiles(&self, user: &str, password: &str) -> Result<bool, CallError> {
        let request = self.client.get(format!("{}/admin/policies/inspect", self.base)).basic_auth(user, Some(password));
        match send_classified(request, "cerbos inspect policies").await {
            Ok(_) => Ok(true),
            Err(e) if e.message.contains("500") && e.message.contains("could not inspect policies") => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub async fn plan(&self, subject: &str, permission: &str, object_type: &str, principal: Option<&Value>) -> Result<Plan, String> {
        let body = json!({
            "requestId": "pg_auth",
            "action": permission,
            "principal": principal_of(subject, principal)?,
            "resource": resource_of(object_type, principal),
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
                let mut resource = resource_of(object_type, principal);
                resource["id"] = json!(id);
                resource["attr"] = attr.cloned().unwrap_or_else(|| json!({}));
                json!({"actions": [permission], "resource": resource})
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
    let mut out = json!({"id": id, "roles": roles, "attr": attr});
    scoped(&mut out, principal);
    Ok(out)
}

/// A resource of `kind` as Cerbos takes it, in the request's scope and policy version: Cerbos picks
/// scoped resource policies by the resource's scope, so without it a scoped override (a deny
/// included) never applies.
fn resource_of(kind: &str, principal: Option<&Value>) -> Value {
    let mut resource = json!({"kind": kind});
    scoped(&mut resource, principal);
    resource
}

/// Copy the request's scope and policy version (from the extension's principal) onto a Cerbos
/// principal or resource.
fn scoped(target: &mut Value, principal: Option<&Value>) {
    if let Some(scope) = principal.and_then(|p| p.get("scope")).and_then(Value::as_str) {
        target["scope"] = json!(scope);
    }
    if let Some(version) = principal.and_then(|p| p.get("policy_version")).and_then(Value::as_str) {
        target["policyVersion"] = json!(version);
    }
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

/// The id Cerbos stores a policy under (`resource.expense.vdefault/acme`), to disable it.
fn policy_id(policy: &Value) -> Option<String> {
    let scoped = |p: &Value| p["scope"].as_str().filter(|s| !s.is_empty()).map(|s| format!("/{s}")).unwrap_or_default();
    let version = |p: &Value| p["version"].as_str().unwrap_or("default").to_owned();
    let named = |kind: &str, field: &str| policy.get(kind).and_then(|p| p[field].as_str().map(|n| (p, n)));
    if let Some((p, n)) = named("resourcePolicy", "resource") {
        Some(format!("resource.{n}.v{}{}", version(p), scoped(p)))
    } else if let Some((p, n)) = named("principalPolicy", "principal") {
        Some(format!("principal.{n}.v{}{}", version(p), scoped(p)))
    } else if let Some((p, n)) = named("rolePolicy", "role") {
        Some(format!("role.{n}.v{}{}", version(p), scoped(p)))
    } else if let Some((_, n)) = named("derivedRoles", "name") {
        Some(format!("derived_roles.{n}"))
    } else if let Some((_, n)) = named("exportVariables", "name") {
        Some(format!("export_variables.{n}"))
    } else {
        named("exportConstants", "name").map(|(_, n)| format!("export_constants.{n}"))
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
