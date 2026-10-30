//! The mapping stored with an engine's rules (`authz.set_rules(engine, name, body, mapping => ...)`):
//! which tables hold which types, how a row is checked, and which rows are relationships, as YAML,
//! compiled into the document `authz_internal.setup` applies (sql/rules.sql,
//! `authz_internal.rules_apply`). The rules stay in the engine's own language; the mapping is the
//! same shape for every engine.
//!
//! ```yaml
//! documents:                       # a table: one entry, or a list of them
//!   type: document                 # default: the table's name, when the rules have that type
//!   id: id                         # default: its primary key; a column or an expression
//!   read: viewer                   # what reading a row asks (a protection needs it)
//!   write: editor                  # and update, delete: what changing one asks
//!   insert: org#can_manage from org_id   # an insert checked on a parent
//!   attributes: [owner, org_id as org]   # the values rules or caveats read
//!   tree: folders(id, parent_id) from viewer_direct   # a permission inherited down a tree
//!   relations:                     # rows that are relationships of this type
//!     org: org_id                  # this row (id) <relation> the column's subject
//!     shared_with: {subject: user_id, until: expires_at, when: "kind = 'user'"}
//!   rows: {object: org_id, relation: role, subject: user_id}   # the relation read from a column
//!                                  # (`allowed: [admin, member]`: the ones a row may name)
//! ```
//!
//! A relation's subject type comes from the rules when only one fits (`subject_type` says which
//! otherwise, `subject_relation` a set like `team#member`, `subject_id` a fixed one like `*`);
//! `object` names another object column, `until` an end, `when` a filter, `condition` an OpenFGA
//! condition or SpiceDB caveat with the row's values (`{name: ..., with: {parameter: column}}`).
//! Rules kept in the engine itself (`body` null) are not read: the mapping names every type.

use crate::rules::{Entry, entries};
use pgrx::prelude::*;
use serde_json::{Map, Value, json};

/// The setup document this mapping describes: `{"relationships": [...], "protect": [...]}`.
#[pg_extern(immutable, parallel_safe, schema = "authz_internal")]
#[expect(clippy::needless_pass_by_value, reason = "pgrx hands text arguments over by value")]
fn mapping_document(engine: String, body: Option<String>, mapping: String) -> pgrx::JsonB {
    match document(&engine, body.as_deref(), &mapping) {
        Ok(d) => pgrx::JsonB(d),
        Err(e) => refuse(format!("pg_auth: {engine} mapping: {e}")),
    }
}

fn refuse(message: String) -> ! {
    ereport!(ERROR, PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, message);
}

pub fn document(engine: &str, body: Option<&str>, mapping: &str) -> Result<Value, String> {
    let model = match body {
        Some(b) => Some(entries(engine, b)?),
        None => None,
    };
    let yaml: Value = serde_norway::from_str(mapping).map_err(|e| format!("not YAML: {e}"))?;
    let tables = match yaml {
        Value::Null => Map::new(),
        Value::Object(m) => m,
        _ => return Err("a map of tables (`documents: {read: viewer}`)".into()),
    };
    let mut protect = Vec::new();
    let mut relationships = Vec::new();
    for (table, entries) in &tables {
        let list = match entries {
            Value::Array(items) => items.clone(),
            one => vec![one.clone()],
        };
        for entry in list {
            let at = |e: String| format!("{table}: {e}");
            let entry = entry.as_object().ok_or_else(|| at("an entry is a map (`read: viewer`)".into()))?;
            if let Some(k) = entry.keys().find(|k| !KEYS.contains(&k.as_str())) {
                return Err(at(format!("no key `{k}` (an entry takes {})", KEYS.join(", "))));
            }
            let ty = type_of(table, entry, model.as_deref()).map_err(at)?;
            if PROTECTION.iter().any(|k| entry.contains_key(*k)) {
                protect.push(protection(table, &ty, entry).map_err(at)?);
            }
            for (relation, spec) in entry.get("relations").and_then(Value::as_object).into_iter().flatten() {
                relationships.push(relation_of(table, &ty, entry, relation, spec, model.as_deref()).map_err(at)?);
            }
            if let Some(rows) = entry.get("rows") {
                relationships.push(rows_of(table, &ty, rows, model.as_deref()).map_err(at)?);
            }
        }
    }
    name_uniquely(&mut relationships, |r| format!("{}_{}", r["object"]["type"].as_str().unwrap_or(""), relation_label(r)));
    // a table protected as two types keeps both, the second under its type's name
    let mut seen = std::collections::HashSet::new();
    for p in &mut protect {
        let table = p["table"].as_str().unwrap_or("").to_owned();
        if !seen.insert(table) {
            p["name"] = json!(sanitize(p["type"].as_str().unwrap_or("")));
        }
    }
    Ok(json!({"relationships": relationships, "protect": protect}))
}

const KEYS: [&str; 11] = ["type", "id", "read", "write", "update", "delete", "insert", "attributes", "tree", "relations", "rows"];
const PROTECTION: [&str; 7] = ["read", "write", "update", "delete", "insert", "attributes", "tree"];
const RELATION_KEYS: [&str; 9] =
    ["subject", "subject_type", "subject_relation", "subject_id", "object", "until", "when", "condition", "name"];

fn text<'a>(entry: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim())),
        Some(other) => Err(format!("`{key}` is a name, not {other}")),
    }
}

/// The entry's type: as it says, or the table's name (singular or plural, as written or
/// capitalized) when the rules have exactly one such type.
fn type_of(table: &str, entry: &Map<String, Value>, model: Option<&[Entry]>) -> Result<String, String> {
    if let Some(t) = text(entry, "type")? {
        if let Some(model) = model
            && !model.iter().any(|e| e.kind == "type" && e.object_type == t)
        {
            return Err(format!("the rules have no type {t} (they have: {})", types(model)));
        }
        return Ok(t.to_owned());
    }
    let Some(model) = model else { return Err("name its type (`type: ...`): the rules are kept in the engine".into()) };
    let base = table.rsplit('.').next().unwrap_or(table);
    let singular = base.strip_suffix("ies").map(|s| format!("{s}y")).or_else(|| base.strip_suffix('s').map(str::to_owned));
    let capital = |s: &str| s.chars().next().map(|c| c.to_uppercase().chain(s.chars().skip(1)).collect::<String>()).unwrap_or_default();
    let mut names: Vec<String> = [Some(base.to_owned()), singular].into_iter().flatten().flat_map(|n| [capital(&n), n]).collect();
    names.dedup();
    let found: Vec<&String> = names.iter().filter(|n| model.iter().any(|e| e.kind == "type" && &e.object_type == *n)).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("name its type (`type: ...`): the rules have none called {base} (they have: {})", types(model))),
        many => {
            Err(format!("name its type (`type: ...`): {} would all do", many.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")))
        }
    }
}

fn types(model: &[Entry]) -> String {
    let mut t: Vec<&str> = model.iter().filter(|e| e.kind == "type").map(|e| e.object_type.as_str()).collect();
    t.dedup();
    t.join(", ")
}

fn protection(table: &str, ty: &str, entry: &Map<String, Value>) -> Result<Value, String> {
    let read = text(entry, "read")?.ok_or("a protected table names what reading a row asks (`read: ...`)")?;
    let mut p = Map::new();
    p.insert("table".into(), json!(table));
    p.insert("type".into(), json!(ty));
    if let Some(id) = text(entry, "id")? {
        p.insert((if is_name(id) { "column" } else { "key" }).into(), json!(id));
    }
    p.insert("select".into(), json!(read));
    for key in ["write", "update", "delete"] {
        if let Some(v) = text(entry, key)? {
            p.insert(key.into(), json!(v));
        }
    }
    if let Some(insert) = entry.get("insert").filter(|v| !v.is_null()) {
        p.insert("insert".into(), insert_of(insert)?);
    }
    if let Some(attrs) = entry.get("attributes").filter(|v| !v.is_null()) {
        p.insert("attributes".into(), attributes_of(attrs)?);
    }
    if let Some(tree) = text(entry, "tree")? {
        // tree: <table>(<id>, <parent>) from <direct permission>
        let (tree, direct) = tree.rsplit_once(" from ").ok_or("write `tree: <table>(<id>, <parent>) from <permission>`")?;
        p.insert("hierarchy".into(), json!({"tree": tree.trim(), "direct": direct.trim()}));
    }
    Ok(Value::Object(p))
}

/// `<type>#<permission> from <column>`, or `{type, permission, from}`.
fn insert_of(v: &Value) -> Result<Value, String> {
    let usage = "write `insert: <type>#<permission> from <column>`";
    let (ty, permission, column) = match v {
        Value::String(s) => {
            let (target, column) = s.split_once(" from ").ok_or(usage)?;
            let (ty, permission) = target.trim().split_once('#').ok_or(usage)?;
            (ty.trim().to_owned(), permission.trim().to_owned(), column.trim().to_owned())
        }
        Value::Object(m) => {
            let get = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned).ok_or(usage);
            (get("type")?, get("permission")?, get("from")?)
        }
        _ => return Err(usage.into()),
    };
    Ok(json!({"type": ty, "permission": permission, "column": column}))
}

/// `[owner, org_id as org]`: the columns sent, each under its own name or another.
fn attributes_of(v: &Value) -> Result<Value, String> {
    let items: Vec<(String, String)> = match v {
        Value::Array(list) => list
            .iter()
            .map(|i| {
                let s = i.as_str().ok_or("an attribute is a column, or `<column> as <name>`")?;
                Ok(match s.split_once(" as ") {
                    Some((column, name)) => (name.trim().to_owned(), column.trim().to_owned()),
                    None => (s.trim().to_owned(), s.trim().to_owned()),
                })
            })
            .collect::<Result<_, String>>()?,
        Value::Object(m) => m
            .iter()
            .map(|(name, col)| Ok((name.clone(), col.as_str().ok_or("an attribute's column is a name")?.to_owned())))
            .collect::<Result<_, String>>()?,
        _ => return Err("attributes are a list (`[owner, org_id as org]`) or a map of name to column".into()),
    };
    let renamed = items.iter().any(|(n, c)| n != c);
    Ok(if renamed {
        Value::Object(items.into_iter().map(|(n, c)| (n, json!(c))).collect())
    } else {
        json!(items.into_iter().map(|(n, _)| n).collect::<Vec<_>>())
    })
}

/// The subject a relationship names: its type (and set relation) as the spec says, or the only
/// one the rules' relation takes.
fn subject_of(
    spec: &Map<String, Value>,
    candidates: &[(String, Option<String>)],
    what: &str,
    model: Option<&[Entry]>,
) -> Result<Map<String, Value>, String> {
    let mut s = match text(spec, "subject_type")? {
        Some(t) => subject_json(t, text(spec, "subject_relation")?.map(str::to_owned).as_ref()),
        None if model.is_none() => {
            return Err(format!("{what}: name its subject's type (`subject_type: ...`): the rules are kept in the engine"));
        }
        None => one_subject(candidates, what)?,
    };
    match (text(spec, "subject")?, text(spec, "subject_id")?) {
        (Some(c), None) => {
            s.insert("column".into(), json!(c));
        }
        (None, Some(id)) => {
            s.insert("id".into(), json!(id));
        }
        (Some(_), Some(_)) => return Err(format!("{what}: a subject is a column (`subject`) or a fixed id (`subject_id`), not both")),
        (None, None) => return Err(format!("{what}: name its subject's column (`subject: ...`) or a fixed id (`subject_id: '*'`)")),
    }
    Ok(s)
}

/// The clauses every relationship takes: an end, a filter, a condition, a name.
fn relationship_clauses(spec: &Map<String, Value>, r: &mut Map<String, Value>) -> Result<(), String> {
    if let Some(until) = text(spec, "until")? {
        r.insert("expires".into(), json!({"column": until}));
    }
    if let Some(when) = text(spec, "when")? {
        r.insert("filter".into(), json!(when));
    }
    if let Some(name) = text(spec, "name")? {
        r.insert("name".into(), json!(name));
    }
    if let Some(c) = spec.get("condition").filter(|v| !v.is_null()) {
        let c = c.as_object().ok_or("a condition is `{name: ..., with: {parameter: column}}`")?;
        let name = c.get("name").and_then(Value::as_str).ok_or("a condition names itself (`name: ...`)")?;
        let context = c.get("with").and_then(Value::as_object).cloned().unwrap_or_default();
        r.insert("condition".into(), json!({"name": name, "context": context}));
    }
    Ok(())
}

/// `<relation>: <subject column>`, or `<relation>: {subject: ..., ...}`: each row this object
/// (the row's id, or `object`) <relation> its subject.
fn relation_of(
    table: &str,
    ty: &str,
    entry: &Map<String, Value>,
    relation: &str,
    spec: &Value,
    model: Option<&[Entry]>,
) -> Result<Value, String> {
    let spec = match spec {
        Value::String(column) => Map::from_iter([("subject".to_owned(), json!(column))]),
        Value::Object(m) => m.clone(),
        _ => return Err(format!("relation {relation}: a subject column, or a map (`{{subject: ..., until: ...}}`)")),
    };
    if let Some(k) = spec.keys().find(|k| !RELATION_KEYS.contains(&k.as_str())) {
        return Err(format!("relation {relation}: no key `{k}` (it takes {})", RELATION_KEYS.join(", ")));
    }
    let what = format!("{ty}#{relation}");
    let entry_in_model = model.and_then(|m| {
        m.iter().find(|e| e.object_type == ty && e.name.as_deref() == Some(relation) && matches!(e.kind, "relation" | "permission"))
    });
    if entry_in_model.is_some_and(|e| e.kind == "permission") {
        return Err(format!("{what} is computed: rows write a relation the rules assign"));
    }
    if model.is_some() && entry_in_model.is_none() {
        return Err(format!("the rules have no relation {what}"));
    }
    let mut r = Map::new();
    r.insert("table".into(), json!(table));
    relationship_clauses(&spec, &mut r)?;
    let s = subject_of(&spec, &entry_in_model.map(bases).unwrap_or_default(), &what, model)?;
    let mut o = Map::new();
    o.insert("type".into(), json!(ty));
    // the object: the column the spec names, else the id the entry is protected by
    if let Some(column) = text(&spec, "object")?.or(text(entry, "id")?.filter(|id| is_name(id))) {
        o.insert("column".into(), json!(column));
    }
    r.insert("object".into(), Value::Object(o));
    r.insert("relation".into(), json!(relation));
    r.insert("subject".into(), Value::Object(s));
    Ok(Value::Object(r))
}

/// `rows: {object, relation, subject}`: each row a relationship of this type, its relation read
/// from a column: one of those the rules give this type that take the subject.
fn rows_of(table: &str, ty: &str, rows: &Value, model: Option<&[Entry]>) -> Result<Value, String> {
    let spec = rows.as_object().ok_or("write `rows: {object: <column>, relation: <column>, subject: <column>}`")?;
    let keys = ["object", "relation", "allowed", "subject", "subject_type", "subject_relation", "until", "when", "condition", "name"];
    if let Some(k) = spec.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(format!("rows: no key `{k}` (it takes {})", keys.join(", ")));
    }
    let object = text(spec, "object")?.ok_or("rows: name the object's column (`object: ...`)")?;
    let relation = text(spec, "relation")?.ok_or("rows: name the relation's column (`relation: ...`)")?;
    let mut r = Map::new();
    r.insert("table".into(), json!(table));
    relationship_clauses(spec, &mut r)?;
    let relations: Vec<&Entry> = model.map(|m| relations_of(m, ty)).unwrap_or_default();
    let candidates: Vec<(String, Option<String>)> = relations.iter().flat_map(|e| bases(e)).collect();
    let s = subject_of(spec, &candidates, &format!("{ty}'s relations"), model)?;
    let mut rel = Map::new();
    rel.insert("column".into(), json!(relation));
    // the relations a row may name: as the mapping lists them, else those taking this subject
    let listed: Option<Vec<String>> = match spec.get("allowed") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            Some(a.iter().map(|v| v.as_str().map(str::to_owned).ok_or("rows: `allowed` lists relations")).collect::<Result<_, _>>()?)
        }
        Some(_) => return Err("rows: `allowed` lists relations (`allowed: [reader, writer]`)".into()),
    };
    if model.is_some() {
        let wanted = (s["type"].as_str().unwrap_or("").to_owned(), s.get("relation").and_then(Value::as_str).map(str::to_owned));
        let taking: Vec<&str> = relations.iter().filter(|e| bases(e).contains(&wanted)).filter_map(|e| e.name.as_deref()).collect();
        if taking.is_empty() {
            return Err(format!("no relation of {ty} takes {}", wanted.0));
        }
        if let Some(r) = listed.iter().flatten().find(|r| !taking.contains(&r.as_str())) {
            return Err(format!("rows: {ty}#{r} does not take {} (these do: {})", wanted.0, taking.join(", ")));
        }
        rel.insert("allowed".into(), listed.map_or_else(|| json!(taking), |l| json!(l)));
    } else if let Some(l) = listed {
        rel.insert("allowed".into(), json!(l));
    }
    r.insert("object".into(), json!({"type": ty, "column": object}));
    r.insert("relation".into(), Value::Object(rel));
    r.insert("subject".into(), Value::Object(s));
    Ok(Value::Object(r))
}

fn relation_label(r: &Value) -> String {
    r["relation"].as_str().map_or_else(|| "rows".to_owned(), str::to_owned)
}

fn name_uniquely(items: &mut [Value], base: impl Fn(&Value) -> String) {
    let mut used: Vec<String> = Vec::new();
    for item in items.iter_mut() {
        if let Some(n) = item.get("name").and_then(Value::as_str) {
            used.push(n.to_owned());
            continue;
        }
        let base = sanitize(&base(item));
        let mut name = base.clone();
        let mut n = 2;
        while used.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        used.push(name.clone());
        item["name"] = json!(name);
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The base subjects a relation's direct types name (`user`, `team#member`), wildcards and the
/// condition or caveat a form needs left aside.
fn bases(e: &Entry) -> Vec<(String, Option<String>)> {
    e.subjects
        .iter()
        .flatten()
        .map(|f| f.split(" with ").next().unwrap_or_default().trim())
        .filter(|f| !f.ends_with(":*"))
        .map(|f| match f.split_once('#') {
            Some((t, rel)) => (t.to_owned(), Some(rel.to_owned())),
            None => (f.to_owned(), None),
        })
        .collect()
}

fn relations_of<'m>(model: &'m [Entry], owner: &str) -> Vec<&'m Entry> {
    model.iter().filter(|e| e.object_type == owner && e.kind == "relation").collect()
}

fn subject_json(ty: &str, rel: Option<&String>) -> Map<String, Value> {
    let mut s = Map::new();
    s.insert("type".into(), json!(ty));
    if let Some(rel) = rel {
        s.insert("relation".into(), json!(rel));
    }
    s
}

fn one_subject(candidates: &[(String, Option<String>)], what: &str) -> Result<Map<String, Value>, String> {
    let mut unique: Vec<&(String, Option<String>)> = Vec::new();
    for c in candidates {
        if !unique.contains(&c) {
            unique.push(c);
        }
    }
    match unique.as_slice() {
        [(ty, rel)] => Ok(subject_json(ty, rel.as_ref())),
        [] => Err(format!("{what} takes no subject a row can write")),
        many => Err(format!(
            "{what} takes {}: say which with `subject_type: ...`",
            many.iter().map(|(t, r)| r.as_ref().map_or_else(|| t.clone(), |r| format!("{t}#{r}"))).collect::<Vec<_>>().join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL: &str = "model\n  schema 1.1\ntype user\ntype team\n  relations\n    define member: [user]\ntype org\n  relations\n    \
        define admin: [user]\n    define member: [user]\n    define can_read: member or admin\n    define can_manage: admin\n\
        type document\n  relations\n    define org: [org]\n    define shared_with: [user, user:*, team#member]\n    \
        define viewer: shared_with or can_read from org\n    define editor: can_manage from org\n";

    const MAPPING: &str = "
documents:
  read: viewer
  write: editor
  insert: org#can_manage from org_id
  relations:
    org: org_id
memberships:
  type: org
  id: org_id
  read: can_read
  write: can_manage
  rows: {object: org_id, relation: role, subject: user_id}
shares:
  - type: document
    id: doc_id
    read: viewer
    write: editor
    relations:
      shared_with: {subject: user_id, subject_type: user, until: expires_at, when: \"kind = 'user'\"}
  - type: document
    relations:
      shared_with: {object: doc_id, subject: team_id, subject_type: team, subject_relation: member, when: \"kind = 'team'\"}
public_docs:
  type: document
  relations:
    shared_with: {object: doc_id, subject_id: '*', subject_type: user}
";

    #[test]
    fn a_mapping_describes_the_tables_of_an_openfga_model() {
        let d = document("openfga", Some(MODEL), MAPPING).unwrap();
        assert_eq!(
            d["protect"][0],
            json!({"table": "documents", "type": "document", "select": "viewer", "write": "editor",
                   "insert": {"type": "org", "permission": "can_manage", "column": "org_id"}}),
            "the type from the table's name; the id left to its primary key"
        );
        let r = d["relationships"].as_array().unwrap();
        assert_eq!(
            r[0],
            json!({"table": "documents", "name": "document_org", "object": {"type": "document"},
                   "relation": "org", "subject": {"type": "org", "column": "org_id"}})
        );
        assert_eq!(
            r[1],
            json!({"table": "memberships", "name": "org_rows", "object": {"type": "org", "column": "org_id"},
                   "relation": {"column": "role", "allowed": ["admin", "member"]}, "subject": {"type": "user", "column": "user_id"}})
        );
        // tables in name order: public_docs, then shares
        assert_eq!(r[2]["subject"], json!({"type": "user", "id": "*"}));
        // the object: the id the entry is protected by
        assert_eq!(r[3]["object"], json!({"type": "document", "column": "doc_id"}));
        assert_eq!((r[3]["filter"].clone(), r[3]["expires"].clone()), (json!("kind = 'user'"), json!({"column": "expires_at"})));
        assert_eq!(r[4]["subject"], json!({"type": "team", "relation": "member", "column": "team_id"}));
        assert_eq!(r[4]["name"], "document_shared_with_3");
    }

    #[test]
    fn a_mapping_says_what_is_wrong_and_where() {
        let wrong = |mapping: &str| document("openfga", Some(MODEL), mapping).unwrap_err();
        assert!(wrong("documents: {read: viewer, colour: red}").contains("documents: no key `colour`"));
        assert!(wrong("folders: {read: viewer}").contains("folders: name its type"), "{}", wrong("folders: {read: viewer}"));
        assert!(wrong("documents: {type: doc, read: viewer}").contains("the rules have no type doc"));
        assert!(wrong("documents: {write: editor}").contains("names what reading a row asks"));
        assert!(wrong("documents: {relations: {viewer: user_id}}").contains("document#viewer is computed"));
        assert!(wrong("documents: {relations: {shared_with: user_id}}").contains("takes user, team#member: say which"));
        assert!(wrong("- documents").contains("a map of tables"));
    }

    #[test]
    fn rules_kept_in_the_engine_take_a_mapping_naming_every_type() {
        let d = document("openfga", None, "documents: {type: document, id: id, read: viewer}").unwrap();
        assert_eq!(d["protect"][0], json!({"table": "documents", "type": "document", "column": "id", "select": "viewer"}));
        let needs = document("openfga", None, "documents: {read: viewer}").unwrap_err();
        assert!(needs.contains("name its type"), "{needs}");
        let subject = document("openfga", None, "shares: {type: document, relations: {shared_with: user_id}}").unwrap_err();
        assert!(subject.contains("subject_type"), "{subject}");
        let rows =
            document("openfga", None, "m: {type: org, rows: {object: o, relation: r, subject: u, subject_type: user, allowed: [admin]}}")
                .unwrap();
        assert_eq!(rows["relationships"][0]["relation"], json!({"column": "r", "allowed": ["admin"]}));
        let listed = document("openfga", Some(MODEL), "m: {type: org, rows: {object: o, relation: r, subject: u, allowed: [can_read]}}")
            .unwrap_err();
        assert!(listed.contains("org#can_read does not take user"), "{listed}");
    }

    #[test]
    fn cedar_and_cerbos_take_the_same_shape() {
        let schema = "entity Org; entity User in [Org]; entity Document in [Org] { org: Org, status?: String };
                      action view appliesTo { principal: User, resource: Document };";
        let d = document(
            "cedar",
            Some(schema),
            "documents: {read: view, attributes: [org_id as org, status], relations: {in: org_id}}
memberships: {type: User, relations: {in: {object: user_id, subject: org_id}}}",
        )
        .unwrap();
        assert_eq!(d["protect"][0]["type"], "Document", "Document from documents, capitalized");
        assert_eq!(d["protect"][0]["attributes"], json!({"org": "org_id", "status": "status"}));
        assert_eq!(d["relationships"][1]["object"], json!({"type": "User", "column": "user_id"}));
        assert_eq!(d["relationships"][1]["subject"], json!({"type": "Org", "column": "org_id"}));
        let policy = "apiVersion: api.cerbos.dev/v1\nresourcePolicy:\n  version: default\n  resource: expense\n  rules: []\n";
        let e = document("cerbos", Some(policy), "expenses: {read: view, attributes: [owner_id as ownerId]}").unwrap();
        assert_eq!(
            e["protect"][0],
            json!({"table": "expenses", "type": "expense", "select": "view", "attributes": {"ownerId": "owner_id"}})
        );
    }
}
