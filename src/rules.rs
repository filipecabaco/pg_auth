//! `authz.rules_names(engine, body)`: what the rules kept in Postgres (sql/rules.sql) define, read
//! from the engine's own language, so a setup naming something they do not have fails when it is
//! committed instead of when the engine is asked (`authz.rules_problems`).
//!
//! One row per type (`type`), per relation a row may write (`relation`, with the subjects it takes:
//! `user`, `user:*`, `team#member`), per relation computed from others (`permission`), and per
//! Cerbos action (`action`, its resource as the type). Rules it cannot read give one `unreadable`
//! row, and are not checked: the engine says what is wrong with them.

use pgrx::prelude::*;
use serde_json::Value;

#[derive(Debug, PartialEq)]
struct Entry {
    object_type: String,
    name: Option<String>,
    kind: &'static str,
    subjects: Option<Vec<String>>,
}

impl Entry {
    fn of_type(t: &str) -> Self {
        Self { object_type: t.to_owned(), name: None, kind: "type", subjects: None }
    }

    fn relation(t: &str, name: &str, subjects: Vec<String>) -> Self {
        let kind = if subjects.is_empty() { "permission" } else { "relation" };
        Self { object_type: t.to_owned(), name: Some(name.to_owned()), kind, subjects: (!subjects.is_empty()).then_some(subjects) }
    }

    fn action(resource: &str, action: &str) -> Self {
        Self { object_type: resource.to_owned(), name: Some(action.to_owned()), kind: "action", subjects: None }
    }
}

#[pg_extern(immutable, parallel_safe)]
#[expect(clippy::type_complexity, reason = "pgrx reads the column names and types from this literal return type")]
fn rules_names(
    engine: &str,
    body: &str,
) -> TableIterator<
    'static,
    (name!(object_type, String), name!(name, Option<String>), name!(kind, String), name!(subjects, Option<Vec<String>>)),
> {
    let names = match engine {
        "openfga" if body.trim_start().starts_with('{') => openfga_json(body),
        "openfga" => Ok(openfga_dsl(body)),
        "spicedb" => Ok(spicedb(body)),
        "cerbos" => cerbos(body),
        _ => Err(format!("no rules for {engine}")),
    };
    let names = names.unwrap_or_else(|e| vec![Entry { object_type: String::new(), name: Some(e), kind: "unreadable", subjects: None }]);
    TableIterator::new(names.into_iter().map(|n| (n.object_type, n.name, n.kind.to_owned(), n.subjects)))
}

/// `type x` and `define y: expr`; a relation with `[...]` takes direct writes.
fn openfga_dsl(dsl: &str) -> Vec<Entry> {
    let mut names = Vec::new();
    let mut current: Option<String> = None;
    for raw in dsl.lines() {
        let line = raw.split(" #").next().unwrap_or_default().trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(t) = line.strip_prefix("type ").or_else(|| line.strip_prefix("extend type ")) {
            let t = t.trim();
            names.push(Entry::of_type(t));
            current = Some(t.to_owned());
        } else if line.starts_with("condition ") || line.starts_with("module ") {
            current = None;
        } else if let (Some(t), Some((name, expr))) = (&current, line.strip_prefix("define ").and_then(|d| d.split_once(':'))) {
            let direct = expr.find('[').and_then(|s| expr[s + 1..].find(']').map(|e| &expr[s + 1..s + 1 + e]));
            let subjects = direct.map(|d| d.split(',').map(subject).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
            names.push(Entry::relation(t, name.trim(), subjects));
        }
    }
    names
}

/// The model's JSON, as OpenFGA's API takes it.
fn openfga_json(body: &str) -> Result<Vec<Entry>, String> {
    let model: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for def in model["type_definitions"].as_array().ok_or("no type_definitions")? {
        let t = def["type"].as_str().ok_or("a type definition without its type")?;
        names.push(Entry::of_type(t));
        for name in def["relations"].as_object().into_iter().flat_map(|r| r.keys()) {
            let direct = def["metadata"]["relations"][name]["directly_related_user_types"].as_array();
            let subjects = direct
                .into_iter()
                .flatten()
                .filter_map(|d| {
                    let t = d["type"].as_str()?;
                    Some(match (d.get("wildcard"), d["relation"].as_str()) {
                        (Some(_), _) => format!("{t}:*"),
                        (None, Some(r)) => format!("{t}#{r}"),
                        (None, None) => t.to_owned(),
                    })
                })
                .collect();
            names.push(Entry::relation(t, name, subjects));
        }
    }
    Ok(names)
}

/// `definition x { relation y: a | b#c  permission z = ... }`; caveats and comments skipped.
fn spicedb(schema: &str) -> Vec<Entry> {
    let text = strip_comments(schema);
    let mut names = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = next_word(rest, &["definition", "caveat"]) {
        let (word, after) = (&rest[at.0..at.1], &rest[at.1..]);
        let Some(open) = after.find('{') else { break };
        let close = matching_brace(after, open).unwrap_or(after.len());
        if word == "definition" {
            let t = after[..open].trim();
            names.push(Entry::of_type(t));
            for line in after[open + 1..close].split(['\n', ';']) {
                let line = line.trim();
                if let Some((name, types)) = line.strip_prefix("relation ").and_then(|r| r.split_once(':')) {
                    names.push(Entry::relation(t, name.trim(), types.split('|').map(subject).filter(|s| !s.is_empty()).collect()));
                } else if let Some((name, _)) = line.strip_prefix("permission ").and_then(|p| p.split_once('=')) {
                    names.push(Entry::relation(t, name.trim(), Vec::new()));
                }
            }
        }
        rest = &after[(close + 1).min(after.len())..];
    }
    names
}

/// The start and end of the first of `words` standing alone in `text`.
fn next_word(text: &str, words: &[&str]) -> Option<(usize, usize)> {
    words
        .iter()
        .filter_map(|w| {
            text.match_indices(w)
                .find(|(i, _)| {
                    let before = text[..*i].chars().next_back();
                    let after = text[i + w.len()..].chars().next();
                    before.is_none_or(char::is_whitespace) && after.is_some_and(char::is_whitespace)
                })
                .map(|(i, _)| (i, i + w.len()))
        })
        .min()
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |e| &after[e + 2..]);
        } else if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |e| &after[e..]);
        } else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// `user`, `user:*`, `team#member`, without a condition or caveat (`user with in_hours`).
fn subject(s: &str) -> String {
    s.split(" with ").next().unwrap_or_default().trim().to_owned()
}

/// A policy's resource and actions: resource policies, role policies (`allowActions`) and
/// principal policies. Derived roles and the rest name none.
fn cerbos(body: &str) -> Result<Vec<Entry>, String> {
    let policy: Value = serde_norway::from_str(body).map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    let strings = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
    if let Some(p) = policy.get("resourcePolicy") {
        let resource = p["resource"].as_str().ok_or("a resource policy without its resource")?;
        names.push(Entry::of_type(resource));
        for rule in p["rules"].as_array().into_iter().flatten() {
            names.extend(strings(&rule["actions"]).iter().map(|a| Entry::action(resource, a)));
        }
    }
    for rule in policy["rolePolicy"]["rules"].as_array().into_iter().flatten() {
        if let Some(resource) = rule["resource"].as_str() {
            names.push(Entry::of_type(resource));
            names.extend(strings(&rule["allowActions"]).iter().map(|a| Entry::action(resource, a)));
        }
    }
    for rule in policy["principalPolicy"]["rules"].as_array().into_iter().flatten() {
        if let Some(resource) = rule["resource"].as_str() {
            names.push(Entry::of_type(resource));
            for action in rule["actions"].as_array().into_iter().flatten().filter_map(|a| a["action"].as_str()) {
                names.push(Entry::action(resource, action));
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(names: &[Entry]) -> Vec<String> {
        names
            .iter()
            .map(|n| match (&n.name, &n.subjects) {
                (None, _) => format!("type {}", n.object_type),
                (Some(name), Some(s)) => format!("{} {}#{name} [{}]", n.kind, n.object_type, s.join(", ")),
                (Some(name), None) => format!("{} {}#{name}", n.kind, n.object_type),
            })
            .collect()
    }

    #[test]
    fn an_openfga_model_names_its_types_relations_and_their_subjects() {
        let dsl = "model\n  schema 1.1\ntype user\ntype doc # a comment\n  relations\n    define owner: [user]\n    \
                   define viewer: [user, user:*, team#member with in_hours] or owner\n    define can_read: viewer but not banned\n\
                   condition in_hours(x: int) {\n  x > 1\n}\n";
        assert_eq!(
            summary(&openfga_dsl(dsl)),
            [
                "type user",
                "type doc",
                "relation doc#owner [user]",
                "relation doc#viewer [user, user:*, team#member]",
                "permission doc#can_read"
            ]
        );
    }

    #[test]
    fn the_json_model_names_the_same() {
        let json = r#"{"schema_version": "1.1", "type_definitions": [{"type": "user"}, {"type": "doc",
            "relations": {"viewer": {"this": {}}, "can_read": {"computedUserset": {"relation": "viewer"}}},
            "metadata": {"relations": {"viewer": {"directly_related_user_types": [{"type": "user"}, {"type": "user", "wildcard": {}},
                {"type": "team", "relation": "member"}]}, "can_read": {}}}}]}"#;
        assert_eq!(
            summary(&openfga_json(json).unwrap()),
            ["type user", "type doc", "permission doc#can_read", "relation doc#viewer [user, user:*, team#member]"]
        );
    }

    #[test]
    fn a_spicedb_schema_names_its_definitions_skipping_caveats_and_comments() {
        let schema = "/** a user */\ndefinition user {}\ncaveat in_hours(x int) {\n  x > 1\n}\n// the docs\n\
                      definition app/doc {\n  relation owner: user\n  relation viewer: user | user:* | team#member with in_hours\n  \
                      permission can_read = viewer + owner // either\n}\n";
        assert_eq!(
            summary(&spicedb(schema)),
            [
                "type user",
                "type app/doc",
                "relation app/doc#owner [user]",
                "relation app/doc#viewer [user, user:*, team#member]",
                "permission app/doc#can_read"
            ]
        );
    }

    #[test]
    fn cerbos_policies_name_their_resource_and_actions() {
        let resource = "apiVersion: api.cerbos.dev/v1\nresourcePolicy:\n  version: default\n  resource: expense\n  rules:\n    \
                        - actions: [\"view\", \"approve:*\"]\n      effect: EFFECT_ALLOW\n      roles: [\"user\"]\n    - actions:\n        - delete\n      \
                        effect: EFFECT_ALLOW\n      roles: [\"admin\"]\n";
        assert_eq!(
            summary(&cerbos(resource).unwrap()),
            ["type expense", "action expense#view", "action expense#approve:*", "action expense#delete"]
        );
        let derived = "apiVersion: api.cerbos.dev/v1\nderivedRoles:\n  name: common\n  definitions: []\n";
        assert_eq!(cerbos(derived).unwrap(), Vec::<Entry>::new());
        assert!(cerbos("resourcePolicy: [unclosed").is_err());
    }
}
