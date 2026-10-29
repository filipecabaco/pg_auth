//! Notes in the engine's rules (authz.set_rules): comments whose text starts with `pg_auth:`, which
//! say where each type's rows live, so one text in the engine's own language is the whole setup.
//! The engine reads them as comments and ignores them; pg_auth reads them into the document
//! `authz.setup` applies (sql/rules.sql, `authz.rules_apply`).
//!
//! A note belongs to the type it sits in: an OpenFGA `type` block, a SpiceDB `definition` (inside
//! its braces, or in the comment just before it), the resource of a Cerbos policy. Three
//! statements, each with clauses after a colon:
//!
//! ```text
//! protects documents (id): read viewer, write editor
//! rows of memberships (org_id, role, user_id)
//! org from documents.org_id
//! ```
//!
//! - `protects <table> (<id>)`: the table's rows are objects of this type, `<id>` a column or an
//!   expression over the row. Clauses: `read <permission>` (required), `write`, `update`,
//!   `delete`, `insert <type>#<permission> from <column>` (checked on a parent), and
//!   `attributes (<column>, <column> as <name>, ...)` (the values Cerbos rules or caveats read).
//! - `rows of <table> (<object>, <relation>, <subject>)`: each row is a relationship, its relation
//!   read from the middle column. The subject's type, and the relations a row may name, come from
//!   the model.
//! - `<relation> from <table>[(<object>)].<subject>`: each row is this relation. The object column
//!   defaults to the id the table is protected by, else its primary key; the subject's type comes
//!   from the model.
//!
//! Relationship clauses: `subject <type>`, `subject <type>#<relation>`, `subject <type>:*` (no
//! subject column), `until <column>` (an end), `condition <name>(<parameter> = <column>, ...)`,
//! `name <name>`, and `when <SQL>`, which takes the rest of the note.

use crate::rules::{Entry, entries};
use pgrx::prelude::*;
use serde_json::{Map, Value, json};

/// The setup document the notes in these rules describe: `{"relationships": [...], "protect": [...]}`.
#[pg_extern(immutable, parallel_safe)]
#[expect(clippy::needless_pass_by_value, reason = "pgrx hands text arguments over by value")]
fn rules_document(engine: String, body: String) -> pgrx::JsonB {
    match document(&engine, &body) {
        Ok(d) => pgrx::JsonB(d),
        Err(e) => refuse(format!("pg_auth: {engine} rules: {e}")),
    }
}

fn refuse(message: String) -> ! {
    ereport!(ERROR, PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, message);
}

struct Note {
    owner: String,
    text: String,
    line: usize,
}

pub fn document(engine: &str, body: &str) -> Result<Value, String> {
    let notes = notes_in(engine, body)?;
    if notes.is_empty() {
        return Ok(json!({"relationships": [], "protect": []}));
    }
    let model = entries(engine, body)?;
    let mut protect: Vec<Value> = Vec::new();
    let mut relationships: Vec<Value> = Vec::new();
    for n in &notes {
        let at = |e: String| format!("line {}: {e}", n.line);
        if let Some(rest) = n.text.strip_prefix("protects ") {
            protect.push(protects(&n.owner, rest).map_err(at)?);
        } else if engine == "cerbos" {
            return Err(at(format!("`{}`: Cerbos keeps no relationships; a note there is `protects <table> (<id>): ...`", n.text)));
        } else if let Some(rest) = n.text.strip_prefix("rows of ") {
            relationships.push(rows_of(&n.owner, rest, &model).map_err(at)?);
        } else if let Some((relation, rest)) = n.text.split_once(" from ").filter(|(r, _)| is_name(r.trim())) {
            relationships.push(relation_from(&n.owner, relation.trim(), rest, &model).map_err(at)?);
        } else {
            return Err(at(format!(
                "`{}`: a note is `protects <table> (<id>): ...`, `rows of <table> (<object>, <relation>, <subject>)` or `<relation> from <table>.<column>`",
                n.text
            )));
        }
    }
    // a relationship's object column: the id its table is protected by, when it is a column
    for r in &mut relationships {
        if r["object"].get("column").is_none()
            && let Some(column) = protect.iter().find(|p| p["table"] == r["table"]).and_then(|p| p.get("column")).cloned()
        {
            r["object"]["column"] = column;
        }
    }
    name_uniquely(&mut relationships, |r| format!("{}_{}", r["object"]["type"].as_str().unwrap_or(""), relation_label(r)));
    // a table protected as two types keeps both, under the second type's name
    let mut seen = std::collections::HashSet::new();
    for p in &mut protect {
        let table = p["table"].as_str().unwrap_or("").to_owned();
        if !seen.insert(table) {
            p["name"] = json!(sanitize(p["type"].as_str().unwrap_or("")));
        }
    }
    Ok(json!({"relationships": relationships, "protect": protect}))
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

// ---- where the notes are

fn notes_in(engine: &str, body: &str) -> Result<Vec<Note>, String> {
    match engine {
        // the JSON model has nowhere to put a comment: keep the notes in the DSL
        "openfga" if body.trim_start().starts_with('{') => Ok(Vec::new()),
        "openfga" => openfga_notes(body),
        "spicedb" => spicedb_notes(body),
        "cerbos" => cerbos_notes(body),
        _ => Err(format!("no rules for {engine}")),
    }
}

/// The text of a `pg_auth:` note, if this comment text is one.
fn note_text(comment: &str) -> Option<String> {
    let c = comment.trim().trim_start_matches(['*', '/']).trim();
    c.strip_prefix("pg_auth:").map(|t| t.trim().to_owned())
}

/// A `#` comment: at the start of the line or after a space (not `team#member`).
fn hash_comment(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    (0..bytes.len()).find(|&i| bytes[i] == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace())).map(|i| &line[i + 1..])
}

fn openfga_notes(dsl: &str) -> Result<Vec<Note>, String> {
    let mut notes = Vec::new();
    let mut owner: Option<String> = None;
    for (i, raw) in dsl.lines().enumerate() {
        let code = raw.split(" #").next().unwrap_or_default().trim();
        if let Some(t) = code.strip_prefix("type ").or_else(|| code.strip_prefix("extend type ")) {
            owner = Some(t.trim().to_owned());
        } else if code.starts_with("condition ") || code.starts_with("module ") {
            owner = None;
        }
        if let Some(text) = hash_comment(raw).and_then(note_text) {
            let owner = owner.clone().ok_or_else(|| format!("line {}: a note belongs inside a type, after `type <name>`", i + 1))?;
            notes.push(Note { owner, text, line: i + 1 });
        }
    }
    Ok(notes)
}

fn spicedb_notes(schema: &str) -> Result<Vec<Note>, String> {
    // comments with where they start, and the schema with them blanked (positions kept)
    let mut comments: Vec<(usize, String)> = Vec::new();
    let mut blank: Vec<u8> = schema.as_bytes().to_vec();
    let bytes = schema.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let end = if bytes[i..].starts_with(b"//") {
            schema[i..].find('\n').map_or(bytes.len(), |e| i + e)
        } else if bytes[i..].starts_with(b"/*") {
            schema[i + 2..].find("*/").map_or(bytes.len(), |e| i + 2 + e + 2)
        } else {
            i += 1;
            continue;
        };
        comments.push((i, schema[i..end].to_owned()));
        for b in &mut blank[i..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        i = end;
    }
    let code = String::from_utf8_lossy(&blank).into_owned();
    // definitions: (start, open brace, close brace, name)
    let mut definitions = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find("definition").map(|p| from + p) {
        let word_start = at == 0 || code.as_bytes()[at - 1].is_ascii_whitespace();
        let after = &code[at + "definition".len()..];
        if word_start && after.starts_with(char::is_whitespace) {
            let open = at + "definition".len() + after.find('{').ok_or("a definition needs its `{`")?;
            let name = code[at + "definition".len()..open].trim().to_owned();
            let close = matching(&code, open).ok_or_else(|| format!("definition {name}: its `{{` needs its `}}`"))?;
            definitions.push((at, open, close, name));
            from = close;
        } else {
            from = at + 1;
        }
    }
    let line_of = |pos: usize| schema[..pos].matches('\n').count() + 1;
    let mut notes = Vec::new();
    for (pos, comment) in comments {
        for (k, line) in comment.trim_start_matches('/').trim_end_matches('/').lines().enumerate() {
            let Some(text) = note_text(line.trim_start_matches('*')) else { continue };
            let owner = definitions
                .iter()
                .find(|(_, open, close, _)| *open < pos && pos < *close)
                .or_else(|| definitions.iter().find(|(start, ..)| *start > pos))
                .map(|(.., name)| name.clone())
                .ok_or_else(|| format!("line {}: a note belongs inside a definition, or just before one", line_of(pos) + k))?;
            notes.push(Note { owner, text, line: line_of(pos) + k });
        }
    }
    Ok(notes)
}

fn cerbos_notes(body: &str) -> Result<Vec<Note>, String> {
    let found: Vec<(usize, String)> =
        body.lines().enumerate().filter_map(|(i, l)| hash_comment(l).and_then(note_text).map(|t| (i + 1, t))).collect();
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let policy: Value = serde_norway::from_str(body).map_err(|e| e.to_string())?;
    let resource = policy["resourcePolicy"]["resource"]
        .as_str()
        .ok_or_else(|| format!("line {}: a note belongs in a resource policy (resourcePolicy.resource)", found[0].0))?;
    Ok(found.into_iter().map(|(line, text)| Note { owner: resource.to_owned(), text, line }).collect())
}

fn matching(text: &str, open: usize) -> Option<usize> {
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

// ---- what a note says

/// The head and the clauses after its first colon (outside parentheses: `id::text` in a key).
fn head_and_clauses(text: &str) -> (&str, Vec<String>) {
    let mut depth = 0;
    let bytes = text.as_bytes();
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ':' if depth == 0 && bytes.get(i + 1) != Some(&b':') && (i == 0 || bytes[i - 1] != b':') => {
                return (text[..i].trim(), clauses(&text[i + 1..]));
            }
            _ => {}
        }
    }
    (text.trim(), Vec::new())
}

/// Clauses split at top-level commas; a `when` clause takes the rest of the note (SQL may hold commas).
fn clauses(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0, false, 0);
    for (i, c) in s.char_indices() {
        if i == start && s[i..].trim_start().starts_with("when ") {
            out.push(s[i..].trim().to_owned());
            return out;
        }
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if depth == 0 && !quoted => {
                out.push(s[start..i].trim().to_owned());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        out.push(last.to_owned());
    }
    out.into_iter().filter(|c| !c.is_empty()).collect()
}

/// `name (inside) after` -> (name, inside, after); no parentheses -> (all, None, "").
fn parenthesized(s: &str) -> Result<(String, Option<String>, String), String> {
    let s = s.trim();
    let Some(open) = s.find('(') else { return Ok((s.to_owned(), None, String::new())) };
    let mut depth = 0;
    for (i, c) in s[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let close = open + i;
                    return Ok((s[..open].trim().to_owned(), Some(s[open + 1..close].trim().to_owned()), s[close + 1..].trim().to_owned()));
                }
            }
            _ => {}
        }
    }
    Err(format!("`{s}`: a `(` needs its `)`"))
}

fn protects(owner: &str, rest: &str) -> Result<Value, String> {
    let (head, clauses) = head_and_clauses(rest);
    let (table, key, after) = parenthesized(head)?;
    let key = key.filter(|k| !k.is_empty()).ok_or_else(|| format!("`protects {head}`: name the id: `protects {table} (<id column>)`"))?;
    if !after.is_empty() {
        return Err(format!("`{after}` after `protects {table} ({key})`: clauses come after a colon"));
    }
    let mut p = Map::new();
    p.insert("table".into(), json!(table));
    p.insert("type".into(), json!(owner));
    p.insert((if is_name(&key) { "column" } else { "key" }).into(), json!(key));
    for c in &clauses {
        let (word, arg) = c.split_once(char::is_whitespace).map_or((c.as_str(), ""), |(w, a)| (w, a.trim()));
        match word {
            "read" | "write" | "update" | "delete" if is_name(arg) => {
                p.insert((if word == "read" { "select" } else { word }).into(), json!(arg));
            }
            "insert" => {
                // insert <type>#<permission> from <column>
                let (target, column) =
                    arg.split_once(" from ").ok_or_else(|| format!("`{c}`: write `insert <type>#<permission> from <column>`"))?;
                let (ty, permission) =
                    target.trim().split_once('#').ok_or_else(|| format!("`{c}`: write `insert <type>#<permission> from <column>`"))?;
                p.insert("insert".into(), json!({"type": ty, "permission": permission, "column": column.trim()}));
            }
            "attributes" => {
                let (_, list, _) = parenthesized(arg)?;
                let list = list.ok_or_else(|| format!("`{c}`: write `attributes (<column>, <column> as <name>)`"))?;
                let items: Vec<(String, String)> = list
                    .split(',')
                    .map(str::trim)
                    .filter(|i| !i.is_empty())
                    .map(|i| match i.split_once(" as ") {
                        Some((column, name)) => (name.trim().to_owned(), column.trim().to_owned()),
                        None => (i.to_owned(), i.to_owned()),
                    })
                    .collect();
                let renamed = items.iter().any(|(n, col)| n != col);
                p.insert(
                    "attributes".into(),
                    if renamed {
                        Value::Object(items.into_iter().map(|(n, col)| (n, json!(col))).collect())
                    } else {
                        json!(items.into_iter().map(|(n, _)| n).collect::<Vec<_>>())
                    },
                );
            }
            _ => return Err(format!("`{c}`: a protection takes read, write, update, delete, insert and attributes")),
        }
    }
    if !p.contains_key("select") {
        return Err(format!("`protects {table} ({key})`: say which permission reads a row: `: read <permission>`"));
    }
    Ok(Value::Object(p))
}

/// A subject as a note writes it: `user`, `team#member`, `user:*`, `platform:main`.
fn subject_of(spec: &str) -> Result<Map<String, Value>, String> {
    let spec = spec.trim();
    let mut s = Map::new();
    if let Some((ty, id)) = spec.split_once(':') {
        s.insert("type".into(), json!(ty));
        s.insert("id".into(), json!(id));
    } else if let Some((ty, relation)) = spec.split_once('#') {
        s.insert("type".into(), json!(ty));
        s.insert("relation".into(), json!(relation));
    } else if is_name(spec) {
        s.insert("type".into(), json!(spec));
    } else {
        return Err(format!("`subject {spec}`: a subject is `<type>`, `<type>#<relation>` or `<type>:*`"));
    }
    Ok(s)
}

/// The relationship clauses into `r`; the subject the note names, if any.
fn relationship_clauses(clauses: &[String], r: &mut Map<String, Value>) -> Result<Option<Map<String, Value>>, String> {
    let mut subject = None;
    for c in clauses {
        let (word, arg) = c.split_once(char::is_whitespace).map_or((c.as_str(), ""), |(w, a)| (w, a.trim()));
        match word {
            "subject" => subject = Some(subject_of(arg)?),
            "until" if is_name(arg) => {
                r.insert("expires".into(), json!({"column": arg}));
            }
            "when" if !arg.is_empty() => {
                r.insert("filter".into(), json!(arg));
            }
            "name" if is_name(arg) => {
                r.insert("name".into(), json!(arg));
            }
            "condition" => {
                let (name, params, _) = parenthesized(arg)?;
                let mut context = Map::new();
                for p in params.unwrap_or_default().split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    let (param, column) = p.split_once('=').ok_or_else(|| format!("`{p}`: write `<parameter> = <column>`"))?;
                    context.insert(param.trim().to_owned(), json!(column.trim()));
                }
                r.insert("condition".into(), json!({"name": name, "context": context}));
            }
            _ => return Err(format!("`{c}`: a relationship takes subject, until, condition, name and when")),
        }
    }
    Ok(subject)
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
            "{what} takes {}: say which with `: subject <type>`",
            many.iter().map(|(t, r)| r.as_ref().map_or_else(|| t.clone(), |r| format!("{t}#{r}"))).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn rows_of(owner: &str, rest: &str, model: &[Entry]) -> Result<Value, String> {
    let (head, clauses) = head_and_clauses(rest);
    let (table, columns, _) = parenthesized(head)?;
    let columns: Vec<String> = columns.unwrap_or_default().split(',').map(|c| c.trim().to_owned()).collect();
    let [object, relation, subject] = columns.as_slice() else {
        return Err(format!("`rows of {head}`: name three columns: `rows of {table} (<object>, <relation>, <subject>)`"));
    };
    let mut r = Map::new();
    r.insert("table".into(), json!(table));
    let named = relationship_clauses(&clauses, &mut r)?;
    let relations = relations_of(model, owner);
    let candidates: Vec<(String, Option<String>)> = relations.iter().flat_map(|e| bases(e)).collect();
    let mut s = match named {
        Some(s) => s,
        None => one_subject(&candidates, &format!("{owner}'s relations"))?,
    };
    // the relations a row may name: those taking this subject
    let wanted = (s["type"].as_str().unwrap_or("").to_owned(), s.get("relation").and_then(Value::as_str).map(str::to_owned));
    let allowed: Vec<&str> = relations.iter().filter(|e| bases(e).contains(&wanted)).filter_map(|e| e.name.as_deref()).collect();
    if allowed.is_empty() {
        return Err(format!("no relation of {owner} takes {}", wanted.0));
    }
    s.insert("column".into(), json!(subject));
    r.insert("object".into(), json!({"type": owner, "column": object}));
    r.insert("relation".into(), json!({"column": relation, "allowed": allowed}));
    r.insert("subject".into(), Value::Object(s));
    Ok(Value::Object(r))
}

fn relation_from(owner: &str, relation: &str, rest: &str, model: &[Entry]) -> Result<Value, String> {
    let (head, clauses) = head_and_clauses(rest);
    let (table, object, after) = parenthesized(head)?;
    // `table(object).subject`, or `table.subject` (the object column left to the table)
    let (table, object, subject_column) = if let Some(o) = object {
        (table, Some(o), after.strip_prefix('.').map(str::to_owned).filter(|c| !c.is_empty()))
    } else {
        let (t, c) =
            table.rsplit_once('.').ok_or_else(|| format!("`{head}`: name the subject's column: `{relation} from <table>.<column>`"))?;
        (t.to_owned(), None, Some(c.to_owned()))
    };
    let mut r = Map::new();
    r.insert("table".into(), json!(table));
    let named = relationship_clauses(&clauses, &mut r)?;
    let entry = model.iter().find(|e| e.object_type == owner && e.name.as_deref() == Some(relation) && e.kind != "type");
    if entry.is_some_and(|e| e.kind == "permission") {
        return Err(format!("{owner}#{relation} is computed: rows write a relation the model assigns"));
    }
    let mut s = match named {
        Some(s) => s,
        None => one_subject(&entry.map(bases).unwrap_or_default(), &format!("{owner}#{relation}"))?,
    };
    match (subject_column, s.contains_key("id")) {
        (Some(c), false) => {
            s.insert("column".into(), json!(c));
        }
        (None, true) => {}
        (Some(_), true) => {
            return Err(format!("`{head}`: a fixed subject (`subject {}:...`) takes no column", s["type"].as_str().unwrap_or("")));
        }
        (None, false) => return Err(format!("`{head}`: name the subject's column, or a fixed subject (`: subject <type>:*`)")),
    }
    let mut o = Map::new();
    o.insert("type".into(), json!(owner));
    if let Some(column) = object {
        o.insert("column".into(), json!(column));
    }
    r.insert("object".into(), Value::Object(o));
    r.insert("relation".into(), json!(relation));
    r.insert("subject".into(), Value::Object(s));
    Ok(Value::Object(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL: &str = "model\n  schema 1.1\ntype user\ntype team\n  relations\n    define member: [user]\ntype org\n  \
        # pg_auth: rows of memberships (org_id, role, user_id)\n  relations\n    define admin: [user]\n    define member: [user]\n    \
        define can_read: member or admin\n    define can_manage: admin\ntype document\n  \
        # pg_auth: protects documents (id): read viewer, write editor\n  # pg_auth: org from documents.org_id\n  \
        # pg_auth: shared_with from shares(doc_id).user_id: subject user, until expires_at, when kind = 'user'\n  \
        # pg_auth: shared_with from shares(doc_id).team_id: subject team#member, when kind = 'team'\n  \
        # pg_auth: shared_with from public_docs(doc_id): subject user:*\n  relations\n    define org: [org]\n    \
        define shared_with: [user, user:*, team#member]\n    define viewer: shared_with or can_read from org\n    define editor: can_manage from org\n";

    #[test]
    fn notes_in_an_openfga_model_describe_its_tables() {
        let d = document("openfga", MODEL).unwrap();
        assert_eq!(
            d["protect"],
            json!([{"table": "documents", "type": "document", "column": "id", "select": "viewer", "write": "editor"}])
        );
        let r = d["relationships"].as_array().unwrap();
        // the subject and the relations a row may name, from the model
        assert_eq!(
            r[0],
            json!({"table": "memberships", "name": "org_rows", "object": {"type": "org", "column": "org_id"},
                   "relation": {"column": "role", "allowed": ["admin", "member"]}, "subject": {"type": "user", "column": "user_id"}})
        );
        // the object column: the id the table is protected by
        assert_eq!(
            r[1],
            json!({"table": "documents", "name": "document_org", "object": {"type": "document", "column": "id"},
                   "relation": "org", "subject": {"type": "org", "column": "org_id"}})
        );
        assert_eq!(r[2]["filter"], "kind = 'user'");
        assert_eq!(r[2]["expires"], json!({"column": "expires_at"}));
        assert_eq!(r[2]["subject"], json!({"type": "user", "column": "user_id"}));
        assert_eq!(r[3]["subject"], json!({"type": "team", "relation": "member", "column": "team_id"}));
        assert_eq!(r[3]["name"], "document_shared_with_2");
        assert_eq!(r[4]["subject"], json!({"type": "user", "id": "*"}));
    }

    #[test]
    fn notes_in_a_spicedb_schema_belong_to_their_definition() {
        let schema = "definition user {}\n\n/**\n * pg_auth: protects documents (id): read viewer\n */\ndefinition document {\n  \
                      // pg_auth: org from documents.org_id\n  relation org: org\n  permission viewer = org->member\n}\n\
                      definition org {\n  // pg_auth: rows of memberships (org_id, role, user_id)\n  relation member: user\n}\n";
        let d = document("spicedb", schema).unwrap();
        assert_eq!(d["protect"][0]["type"], "document");
        assert_eq!(d["relationships"][0]["object"], json!({"type": "document", "column": "id"}));
        assert_eq!(d["relationships"][1]["relation"], json!({"column": "role", "allowed": ["member"]}));
    }

    #[test]
    fn notes_in_a_cerbos_policy_protect_its_resource() {
        let policy = "# pg_auth: protects expenses (id): read view, write update, attributes (owner_id as ownerId, status)\n\
                      apiVersion: api.cerbos.dev/v1\nresourcePolicy:\n  version: default\n  resource: expense\n  rules: []\n";
        let d = document("cerbos", policy).unwrap();
        assert_eq!(
            d["protect"][0],
            json!({"table": "expenses", "type": "expense", "column": "id", "select": "view", "write": "update",
                   "attributes": {"ownerId": "owner_id", "status": "status"}})
        );
        let refused = document(
            "cerbos",
            &policy.replace("protects expenses (id): read view, write update, attributes (owner_id as ownerId, status)", "org from x.y"),
        );
        assert!(refused.unwrap_err().contains("Cerbos keeps no relationships"));
    }

    #[test]
    fn rules_without_notes_describe_nothing_and_unclear_notes_say_why() {
        assert_eq!(document("openfga", "model\n  schema 1.1\ntype user\n").unwrap(), json!({"relationships": [], "protect": []}));
        let fails = |note: &str| {
            document("openfga", &format!("model\n  schema 1.1\ntype user\ntype doc\n  # pg_auth: {note}\n  relations\n    define owner: [user]\n    define viewer: [user, team#member]\n    define can_read: owner\n"))
                .unwrap_err()
        };
        assert!(fails("protects docs (id)").contains("say which permission reads a row"));
        assert!(fails("can_read from docs.user_id").contains("doc#can_read is computed"));
        assert!(fails("viewer from shares(doc_id).who").contains("viewer takes user, team#member: say which"));
        assert!(fails("rows of docs (id, rel)").contains("name three columns"));
        assert!(fails("whatever").contains("a note is"));
        assert!(
            document("openfga", "model\n  schema 1.1\n# pg_auth: protects x (id): read y\ntype user\n")
                .unwrap_err()
                .contains("belongs inside a type")
        );
    }
}
