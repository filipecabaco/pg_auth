//! OpenFGA's modeling language (the DSL people write and read) to the JSON its API takes, so the
//! model kept in Postgres (authz.set_rules) can be written to the store as is. Covers schema 1.1:
//! types, relations, directly related types (`user`, `user:*`, `team#member`), computed
//! relations, `X from Y`, `or`, `and`, `but not` and parentheses. Conditions and modules are
//! refused, as pg_auth sends no context for them.

use serde_json::{Map, Value, json};

/// The model's JSON (`{"schema_version", "type_definitions"}`), as `fga model transform` gives it.
pub fn to_json(dsl: &str) -> Result<Value, String> {
    let mut schema = None;
    let mut types: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for (n, raw) in dsl.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() || line == "model" || line == "relations" {
            continue;
        }
        let at = |e: String| format!("line {}: {e}", n + 1);
        if let Some(v) = line.strip_prefix("schema ") {
            schema = Some(v.trim().to_owned());
        } else if let Some(name) = line.strip_prefix("type ") {
            types.push((name.trim().to_owned(), Vec::new()));
        } else if let Some(def) = line.strip_prefix("define ") {
            let (name, expr) = def.split_once(':').ok_or_else(|| at("define needs `name: expression`".into()))?;
            let (_, relations) = types.last_mut().ok_or_else(|| at("define outside a type".into()))?;
            relations.push((name.trim().to_owned(), expr.trim().to_owned()));
        } else if line.starts_with("condition ") || line.starts_with("module ") || line.starts_with("extend type ") {
            return Err(at("conditions and modules are not supported: pg_auth sends no context for them".into()));
        } else {
            return Err(at(format!("unexpected `{line}`")));
        }
    }
    let schema = schema.ok_or("the model has no `schema` line")?;
    if schema != "1.1" {
        return Err(format!("schema {schema}: only 1.1 is supported"));
    }
    let mut definitions = Vec::new();
    for (name, relations) in types {
        if relations.is_empty() {
            definitions.push(json!({"type": name}));
            continue;
        }
        let mut rewrites = Map::new();
        let mut metadata = Map::new();
        for (relation, expr) in relations {
            let mut direct = Vec::new();
            let node = Parser::new(&expr).parse(&mut direct).map_err(|e| format!("type {name}, relation {relation}: {e}"))?;
            rewrites.insert(relation.clone(), node);
            let meta = if direct.is_empty() { json!({}) } else { json!({"directly_related_user_types": direct}) };
            metadata.insert(relation, meta);
        }
        definitions.push(json!({"type": name, "relations": rewrites, "metadata": {"relations": metadata}}));
    }
    Ok(json!({"schema_version": schema, "type_definitions": definitions}))
}

/// A line without its comment: `#` at the start or after a space (not `team#member`).
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    (0..bytes.len()).find(|&i| bytes[i] == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace())).map_or(line, |i| &line[..i])
}

struct Parser {
    tokens: Vec<String>,
    at: usize,
}

impl Parser {
    fn new(expr: &str) -> Self {
        let mut tokens = Vec::new();
        let mut rest = expr.trim();
        while !rest.is_empty() {
            let (token, after) = if let Some(after) = rest.strip_prefix('[') {
                let end = after.find(']').map_or(after.len(), |e| e + 1);
                (format!("[{}", &after[..end]), &after[end..])
            } else if rest.starts_with('(') || rest.starts_with(')') {
                (rest[..1].to_owned(), &rest[1..])
            } else {
                let end = rest.find(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '[').unwrap_or(rest.len());
                (rest[..end].to_owned(), &rest[end..])
            };
            tokens.push(token);
            rest = after.trim_start();
        }
        Self { tokens, at: 0 }
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(String::as_str)
    }

    fn next(&mut self) -> Result<String, String> {
        let t = self.tokens.get(self.at).cloned().ok_or("the expression ends too soon")?;
        self.at += 1;
        Ok(t)
    }

    fn parse(mut self, direct: &mut Vec<Value>) -> Result<Value, String> {
        let node = self.expression(direct)?;
        match self.peek() {
            None => Ok(node),
            Some(t) => Err(format!("unexpected `{t}`")),
        }
    }

    /// operand (or|and operand)* [but not operand]: one operator per level, as the DSL requires;
    /// mixing them needs parentheses.
    fn expression(&mut self, direct: &mut Vec<Value>) -> Result<Value, String> {
        let first = self.operand(direct)?;
        let mut children = vec![first];
        let mut op: Option<String> = None;
        while let Some(t) = self.peek().filter(|t| *t == "or" || *t == "and") {
            let t = t.to_owned();
            if op.as_deref().is_some_and(|o| o != t) {
                return Err("`or` and `and` together need parentheses".into());
            }
            self.at += 1;
            children.push(self.operand(direct)?);
            op = Some(t);
        }
        let mut node = match op.as_deref() {
            Some("or") => json!({"union": {"child": children}}),
            Some(_) => json!({"intersection": {"child": children}}),
            None => children.remove(0),
        };
        if self.peek() == Some("but") {
            self.at += 1;
            if self.next()? != "not" {
                return Err("`but` must be `but not`".into());
            }
            let subtract = self.operand(direct)?;
            node = json!({"difference": {"base": node, "subtract": subtract}});
        }
        Ok(node)
    }

    fn operand(&mut self, direct: &mut Vec<Value>) -> Result<Value, String> {
        let t = self.next()?;
        if let Some(list) = t.strip_prefix('[') {
            let list = list.strip_suffix(']').ok_or("a type list needs its `]`")?;
            for item in list.split(',').map(str::trim).filter(|i| !i.is_empty()) {
                direct.push(directly_related(item)?);
            }
            return Ok(json!({"this": {}}));
        }
        if t == "(" {
            let inner = self.expression(direct)?;
            return if self.next()? == ")" { Ok(inner) } else { Err("a `(` needs its `)`".into()) };
        }
        if self.peek() == Some("from") {
            self.at += 1;
            let tupleset = self.next()?;
            return Ok(json!({"tupleToUserset": {"tupleset": {"relation": tupleset}, "computedUserset": {"relation": t}}}));
        }
        Ok(json!({"computedUserset": {"relation": t}}))
    }
}

/// `user`, `user:*` (anyone of the type) or `team#member` (a set of subjects).
fn directly_related(item: &str) -> Result<Value, String> {
    if item.contains(" with ") {
        return Err(format!("`{item}`: conditions are not supported: pg_auth sends no context for them"));
    }
    Ok(if let Some(t) = item.strip_suffix(":*") {
        json!({"type": t, "wildcard": {}})
    } else if let Some((t, relation)) = item.split_once('#') {
        json!({"type": t, "relation": relation})
    } else {
        json!({"type": item})
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // `fga model transform` (openfga/cli) on the demo's model: the JSON must be the same
    #[test]
    fn the_demo_model_converts_as_the_cli_does() {
        let dsl = "model\n  schema 1.1\ntype user\ntype org\n  relations\n    define admin: [user]\n    define member: [user]\n    \
                   define can_manage: admin\n    define can_read: member or admin\ntype document\n  relations\n    define org: [org]\n    \
                   define shared_with: [user]\n    define viewer: shared_with or can_read from org\n    define editor: can_manage from org\n";
        let cli = json!({"schema_version": "1.1", "type_definitions": [
            {"type": "user"},
            {"type": "org",
             "metadata": {"relations": {"admin": {"directly_related_user_types": [{"type": "user"}]}, "can_manage": {}, "can_read": {},
                                        "member": {"directly_related_user_types": [{"type": "user"}]}}},
             "relations": {"admin": {"this": {}}, "can_manage": {"computedUserset": {"relation": "admin"}},
                           "can_read": {"union": {"child": [{"computedUserset": {"relation": "member"}}, {"computedUserset": {"relation": "admin"}}]}},
                           "member": {"this": {}}}},
            {"type": "document",
             "metadata": {"relations": {"editor": {}, "org": {"directly_related_user_types": [{"type": "org"}]},
                                        "shared_with": {"directly_related_user_types": [{"type": "user"}]}, "viewer": {}}},
             "relations": {"editor": {"tupleToUserset": {"computedUserset": {"relation": "can_manage"}, "tupleset": {"relation": "org"}}},
                           "org": {"this": {}}, "shared_with": {"this": {}},
                           "viewer": {"union": {"child": [{"computedUserset": {"relation": "shared_with"}},
                               {"tupleToUserset": {"computedUserset": {"relation": "can_read"}, "tupleset": {"relation": "org"}}}]}}}}]});
        assert_eq!(to_json(dsl).unwrap(), cli);
    }

    // the CLI on wildcards, sets of subjects, `but not` over a parenthesised union, and `and`
    #[test]
    fn exclusion_intersection_and_wildcards_convert_as_the_cli_does() {
        let dsl = "model\n  schema 1.1\ntype repo # a comment\n  relations\n    define reader: [user, user:*, team#member]\n    \
                   define writer: [user]\n    define banned: [user]\n    define organization: [org]\n    \
                   define can_read: (reader or writer or owner from organization) but not banned\n    define both: reader and writer\n";
        let repo = &to_json(dsl).unwrap()["type_definitions"][0];
        let cli = json!({"metadata": {"relations": {"banned": {"directly_related_user_types": [{"type": "user"}]}, "both": {}, "can_read": {},
                "organization": {"directly_related_user_types": [{"type": "org"}]},
                "reader": {"directly_related_user_types": [{"type": "user"}, {"type": "user", "wildcard": {}}, {"relation": "member", "type": "team"}]},
                "writer": {"directly_related_user_types": [{"type": "user"}]}}},
            "relations": {"banned": {"this": {}},
                "both": {"intersection": {"child": [{"computedUserset": {"relation": "reader"}}, {"computedUserset": {"relation": "writer"}}]}},
                "can_read": {"difference": {"base": {"union": {"child": [{"computedUserset": {"relation": "reader"}}, {"computedUserset": {"relation": "writer"}},
                    {"tupleToUserset": {"computedUserset": {"relation": "owner"}, "tupleset": {"relation": "organization"}}}]}},
                    "subtract": {"computedUserset": {"relation": "banned"}}}},
                "organization": {"this": {}}, "reader": {"this": {}}, "writer": {"this": {}}},
            "type": "repo"});
        assert_eq!(repo, &cli);
    }

    #[test]
    fn conditions_mixed_operators_and_stray_text_are_refused() {
        let with = |def: &str| to_json(&format!("model\n  schema 1.1\ntype doc\n  relations\n    define {def}\n"));
        assert!(with("viewer: [user with in_hours]").unwrap_err().contains("conditions are not supported"));
        assert!(with("viewer: a or b and c").unwrap_err().contains("need parentheses"));
        assert!(with("viewer: (a or b").unwrap_err().contains("ends too soon"));
        assert!(to_json("model\n  schema 1.1\ncondition in_hours(x: int) { x > 1 }").unwrap_err().contains("not supported"));
        assert!(to_json("type doc").unwrap_err().contains("no `schema`"));
    }
}
