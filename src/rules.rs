//! `authz_internal.rules_names(engine, body)`: what the rules kept in Postgres (sql/rules.sql) define, read
//! from the engine's own language, so a setup naming something they do not have fails when it is
//! committed instead of when the engine is asked (`authz_internal.rules_problems`).
//!
//! One row per type (`type`), per relation a row may write (`relation`, with each form of subject
//! it takes, as written: `user`, `user:*`, `team#member`, `user with in_region`, `user with
//! expiration`), per relation computed from others (`permission`), per OpenFGA condition or
//! SpiceDB caveat (`condition`, with its parameters), and per Cerbos or Cedar action (`action`, its
//! resource as the type). A Cedar schema's `entity User in [Group]` is a relation `in` taking those
//! types; its policies define nothing to check. Rules it cannot read give one `unreadable` row, and
//! are not checked: the engine says what is wrong with them.

use pgrx::prelude::*;
use serde_json::Value;

#[derive(Debug, PartialEq)]
pub(crate) struct Entry {
    pub(crate) object_type: String,
    pub(crate) name: Option<String>,
    pub(crate) kind: &'static str,
    pub(crate) subjects: Option<Vec<String>>,
}

impl Entry {
    fn of_type(t: &str) -> Self {
        Self { object_type: t.to_owned(), name: None, kind: "type", subjects: None }
    }

    fn relation(t: &str, name: &str, subjects: Vec<String>) -> Self {
        Self { object_type: t.to_owned(), name: Some(name.to_owned()), kind: "relation", subjects: Some(subjects) }
    }

    fn permission(t: &str, name: &str) -> Self {
        Self { object_type: t.to_owned(), name: Some(name.to_owned()), kind: "permission", subjects: None }
    }

    fn condition(name: &str, parameters: Vec<String>) -> Self {
        Self { object_type: String::new(), name: Some(name.to_owned()), kind: "condition", subjects: Some(parameters) }
    }

    fn action(resource: &str, action: &str) -> Self {
        Self { object_type: resource.to_owned(), name: Some(action.to_owned()), kind: "action", subjects: None }
    }
}

#[pg_extern(immutable, parallel_safe, schema = "authz_internal")]
#[expect(clippy::type_complexity, reason = "pgrx reads the column names and types from this literal return type")]
fn rules_names(
    engine: &str,
    body: &str,
) -> TableIterator<
    'static,
    (name!(object_type, String), name!(name, Option<String>), name!(kind, String), name!(subjects, Option<Vec<String>>)),
> {
    let names = entries(engine, body)
        .unwrap_or_else(|e| vec![Entry { object_type: String::new(), name: Some(e), kind: "unreadable", subjects: None }]);
    TableIterator::new(names.into_iter().map(|n| (n.object_type, n.name, n.kind.to_owned(), n.subjects)))
}

/// What these rules define, read from the engine's own language.
pub(crate) fn entries(engine: &str, body: &str) -> Result<Vec<Entry>, String> {
    match engine {
        "openfga" if body.trim_start().starts_with('{') => openfga_json(body),
        "openfga" => Ok(openfga_dsl(body)),
        "spicedb" => Ok(spicedb(body)),
        "cerbos" => cerbos(body),
        "cedar" => cedar(body),
        _ => Err(format!("no rules for {engine}")),
    }
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
        } else if let Some(header) = line.strip_prefix("condition ") {
            // `condition in_region(region: string, allowed: list<string>) {`
            if let Some((name, params)) = header.split_once('(') {
                let params = params.split(')').next().unwrap_or_default();
                names.push(Entry::condition(name.trim(), parameters(params, ':')));
            }
            current = None;
        } else if line.starts_with("module ") {
            current = None;
        } else if let (Some(t), Some((name, expr))) = (&current, line.strip_prefix("define ").and_then(|d| d.split_once(':'))) {
            let direct = expr.find('[').and_then(|s| expr[s + 1..].find(']').map(|e| &expr[s + 1..s + 1 + e]));
            names.push(match direct {
                Some(d) => Entry::relation(t, name.trim(), forms(d.split(','))),
                None => Entry::permission(t, name.trim()),
            });
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
            let Some(direct) = def["metadata"]["relations"][name]["directly_related_user_types"].as_array().filter(|d| !d.is_empty())
            else {
                names.push(Entry::permission(t, name));
                continue;
            };
            let subjects = direct
                .iter()
                .filter_map(|d| {
                    let t = d["type"].as_str()?;
                    let subject = match (d.get("wildcard"), d["relation"].as_str()) {
                        (Some(_), _) => format!("{t}:*"),
                        (None, Some(r)) => format!("{t}#{r}"),
                        (None, None) => t.to_owned(),
                    };
                    Some(match d.get("condition").and_then(Value::as_str).filter(|c| !c.is_empty()) {
                        Some(c) => format!("{subject} with {c}"),
                        None => subject,
                    })
                })
                .collect();
            names.push(Entry::relation(t, name, subjects));
        }
    }
    for (name, condition) in model["conditions"].as_object().into_iter().flatten() {
        names.push(Entry::condition(name, condition["parameters"].as_object().map(|p| p.keys().cloned().collect()).unwrap_or_default()));
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
        if word == "caveat" {
            // `caveat in_region(region string, allowed list<string>) {`
            if let Some((name, params)) = after[..open].split_once('(') {
                names.push(Entry::condition(name.trim(), parameters(params.trim().trim_end_matches(')'), ' ')));
            }
        } else {
            let t = after[..open].trim();
            names.push(Entry::of_type(t));
            for line in after[open + 1..close].split(['\n', ';']) {
                let line = line.trim();
                if let Some((name, types)) = line.strip_prefix("relation ").and_then(|r| r.split_once(':')) {
                    names.push(Entry::relation(t, name.trim(), forms(types.split('|'))));
                } else if let Some((name, _)) = line.strip_prefix("permission ").and_then(|p| p.split_once('=')) {
                    names.push(Entry::permission(t, name.trim()));
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

/// Each form of subject a relation takes, as written, its spacing normalised: `user`,
/// `team#member`, `user with in_region`, `user with in_region and expiration`.
fn forms<'a>(subjects: impl Iterator<Item = &'a str>) -> Vec<String> {
    subjects.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty()).collect()
}

/// A condition's parameter names: `region: string, allowed: list<string>` (OpenFGA, `separator`
/// ':') or `region string, allowed list<string>` (SpiceDB, ' '). Commas inside `<...>` do not
/// split.
fn parameters(params: &str, separator: char) -> Vec<String> {
    let (mut names, mut depth, mut start) = (Vec::new(), 0, 0);
    let mut push = |part: &str| {
        if let Some(name) = part.trim().split(separator).next().map(str::trim).filter(|n| !n.is_empty()) {
            names.push(name.to_owned());
        }
    };
    for (i, c) in params.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                push(&params[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(&params[start..]);
    names
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

/// A Cedar text with its comments blanked out (positions kept, newlines too), and the comments with
/// where each starts: `//` to the end of the line, `/* */`, string literals left alone.
pub(crate) fn cedar_comments(text: &str) -> (String, Vec<(usize, String)>) {
    let bytes = text.as_bytes();
    let mut code = bytes.to_vec();
    let mut comments = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // a string: to its closing quote, escapes skipped
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            continue;
        }
        let end = if bytes[i..].starts_with(b"//") {
            text[i..].find('\n').map_or(bytes.len(), |e| i + e)
        } else if bytes[i..].starts_with(b"/*") {
            text[i + 2..].find("*/").map_or(bytes.len(), |e| i + 2 + e + 2)
        } else {
            i += 1;
            continue;
        };
        comments.push((i, text[i..end].to_owned()));
        for b in &mut code[i..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        i = end;
    }
    (String::from_utf8_lossy(&code).into_owned(), comments)
}

/// A Cedar text's top-level statements (split at `;` outside braces, brackets and strings), each with
/// where it starts, annotations (`@doc("...")`) dropped from its front.
pub(crate) fn cedar_statements(code: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0i32, false, 0);
    for (i, c) in code.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '{' | '[' | '(' if !quoted => depth += 1,
            '}' | ']' | ')' if !quoted => depth -= 1,
            ';' if !quoted && depth == 0 => {
                out.push((start, code[start..i].to_owned()));
                start = i + 1;
            }
            _ => {}
        }
    }
    if !code[start..].trim().is_empty() {
        out.push((start, code[start..].to_owned()));
    }
    out.into_iter()
        .map(|(at, s)| {
            let mut rest = s.trim_start();
            // annotations: @name or @name("...")
            while let Some(after) = rest.strip_prefix('@') {
                let name_end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
                rest = after[name_end..].trim_start();
                if rest.starts_with('(') {
                    rest = rest.find(')').map_or("", |e| &rest[e + 1..]).trim_start();
                }
            }
            (at + (s.len() - s.trim_start().len()), rest.to_owned())
        })
        .filter(|(_, s)| !s.is_empty())
        .collect()
}

/// A comma-separated list of names (`A, B` or `[A, B]`), quotes removed.
fn cedar_names(list: &str) -> Vec<String> {
    list.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|n| n.trim().trim_matches('"').to_owned())
        .filter(|n| !n.is_empty())
        .collect()
}

/// The word `w` in `s` outside strings, as a whole word: its position.
fn cedar_word(s: &str, w: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        }
        let word = |at: usize| at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        let ends = |at: usize| bytes.get(at + w.len()).is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_'));
        if !quoted && s[i..].starts_with(w) && word(i) && ends(i) {
            return Some(i);
        }
    }
    None
}

/// A Cedar schema's entity types (with their `in` parents) and actions (on each resource type they
/// apply to). Policies define none.
fn cedar(body: &str) -> Result<Vec<Entry>, String> {
    let (code, _) = cedar_comments(body);
    let mut names = Vec::new();
    for (_, statement) in cedar_statements(&code) {
        let s = statement.trim();
        if s.starts_with("permit") || s.starts_with("forbid") {
            return Ok(Vec::new());
        }
        if let Some(rest) = s.strip_prefix("namespace") {
            return Err(format!(
                "namespace {}: pg_auth takes Cedar types without a namespace",
                rest.trim().split(['{', ' ']).next().unwrap_or_default()
            ));
        }
        if let Some(rest) = s.strip_prefix("entity ") {
            // entity A, B in [X, Y] = { ... } | entity A enum [...]
            let head = rest.split(['{', '=']).next().unwrap_or_default();
            let head = cedar_word(head, "enum").map_or(head, |e| &head[..e]);
            let (types, parents) = match cedar_word(head, "in") {
                Some(at) => (&head[..at], cedar_names(&head[at + 2..])),
                None => (head, Vec::new()),
            };
            for t in cedar_names(types) {
                names.push(Entry::of_type(&t));
                if !parents.is_empty() {
                    names.push(Entry::relation(&t, "in", parents.clone()));
                }
            }
        } else if let Some(rest) = s.strip_prefix("action ") {
            let Some(applies) = cedar_word(rest, "appliesTo") else { continue };
            let head = &rest[..applies];
            let actions = cedar_names(cedar_word(head, "in").map_or(head, |at| &head[..at]));
            let block = &rest[applies + "appliesTo".len()..];
            let resources = cedar_word(block, "resource")
                .map(|at| {
                    let after = block[at + "resource".len()..].trim_start().trim_start_matches(':').trim_start();
                    let end = if after.starts_with('[') {
                        after.find(']').map_or(after.len(), |e| e + 1)
                    } else {
                        after.find([',', '}']).unwrap_or(after.len())
                    };
                    cedar_names(&after[..end])
                })
                .unwrap_or_default();
            for r in &resources {
                names.extend(actions.iter().map(|a| Entry::action(r, a)));
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
                   condition in_hours(x: int, hours: map<list<int>>) {\n  x > 1\n}\n";
        assert_eq!(
            summary(&openfga_dsl(dsl)),
            [
                "type user",
                "type doc",
                "relation doc#owner [user]",
                "relation doc#viewer [user, user:*, team#member with in_hours]",
                "permission doc#can_read",
                "condition #in_hours [x, hours]"
            ]
        );
    }

    #[test]
    fn the_json_model_names_the_same() {
        let json = r#"{"schema_version": "1.1", "type_definitions": [{"type": "user"}, {"type": "doc",
            "relations": {"viewer": {"this": {}}, "can_read": {"computedUserset": {"relation": "viewer"}}},
            "metadata": {"relations": {"viewer": {"directly_related_user_types": [{"type": "user"}, {"type": "user", "wildcard": {}},
                {"type": "team", "relation": "member", "condition": "in_hours"}]}, "can_read": {}}}}],
            "conditions": {"in_hours": {"name": "in_hours", "expression": "x > 1", "parameters": {"x": {"type_name": "TYPE_NAME_INT"}}}}}"#;
        assert_eq!(
            summary(&openfga_json(json).unwrap()),
            [
                "type user",
                "type doc",
                "permission doc#can_read",
                "relation doc#viewer [user, user:*, team#member with in_hours]",
                "condition #in_hours [x]"
            ]
        );
    }

    #[test]
    fn a_spicedb_schema_names_its_definitions_and_caveats_skipping_comments() {
        let schema = "/** a user */\ndefinition user {}\ncaveat in_hours(x int, hours map<list<int>>) {\n  x > 1\n}\n// the docs\n\
                      definition app/doc {\n  relation owner: user\n  relation viewer: user with expiration | user:* | team#member with in_hours and expiration\n  \
                      permission can_read = viewer + owner // either\n}\n";
        assert_eq!(
            summary(&spicedb(schema)),
            [
                "type user",
                "condition #in_hours [x, hours]",
                "type app/doc",
                "relation app/doc#owner [user]",
                "relation app/doc#viewer [user with expiration, user:*, team#member with in_hours and expiration]",
                "permission app/doc#can_read"
            ]
        );
    }

    #[test]
    fn a_cedar_schema_names_types_parents_and_actions() {
        let schema = r#"
            // pg_auth: a note, not a declaration; entity Fake;
            entity Role;
            @doc("people")
            entity User in [Group, Role] { dept: String, "odd name"?: Long };
            entity Group, Team;
            entity Folder in Folder;
            entity Document in [Folder] = { owner: User, tags: Set<String> };
            /* entity Hidden; */
            action view, "edit doc" appliesTo { principal: [User], resource: [Document, Folder], context: { ip: String } };
            action audit in [view] appliesTo { principal: User, resource: Document };
        "#;
        assert_eq!(
            summary(&cedar(schema).unwrap()),
            [
                "type Role",
                "type User",
                "relation User#in [Group, Role]",
                "type Group",
                "type Team",
                "type Folder",
                "relation Folder#in [Folder]",
                "type Document",
                "relation Document#in [Folder]",
                "action Document#view",
                "action Document#edit doc",
                "action Folder#view",
                "action Folder#edit doc",
                "action Document#audit",
            ]
        );
        assert_eq!(
            cedar(r#"permit (principal, action == Action::"view", resource) when { resource.a == ";" };"#).unwrap(),
            Vec::<Entry>::new()
        );
        assert!(cedar("namespace App { entity User; }").unwrap_err().contains("namespace App"));
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
