//! OpenFGA's modeling language (the DSL people write and read) to the JSON its API takes, so the
//! model kept in Postgres (authz.set_rules) can be written to the store as is. Covers schema 1.1:
//! types, relations, directly related types (`user`, `user:*`, `team#member`, `user with
//! in_region`), computed relations, `X from Y`, `or`, `and`, `but not`, parentheses, and
//! conditions (`condition in_region(region: string, allowed: list<string>) { region in allowed }`).
//! Modules are refused.

use serde_json::{Map, Value, json};

/// The model's JSON (`{"schema_version", "type_definitions"}`), as `fga model transform` gives it.
pub fn to_json(dsl: &str) -> Result<Value, String> {
    let (dsl, conditions) = take_conditions(dsl)?;
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
        } else if line.starts_with("module ") || line.starts_with("extend type ") {
            return Err(at("modules are not supported: keep the model in one text".into()));
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
    let mut model = json!({"schema_version": schema, "type_definitions": definitions});
    if !conditions.is_empty() {
        model["conditions"] = Value::Object(conditions);
    }
    Ok(model)
}

/// The model without its `condition name(params) { expression }` blocks (blank lines in their
/// place, so line numbers still hold), and those conditions as the API takes them.
fn take_conditions(dsl: &str) -> Result<(String, Map<String, Value>), String> {
    let mut rest = String::with_capacity(dsl.len());
    let mut conditions = Map::new();
    let mut at = 0;
    while at < dsl.len() {
        let line_end = dsl[at..].find('\n').map_or(dsl.len(), |e| at + e + 1);
        if !strip_comment(&dsl[at..line_end]).trim_start().starts_with("condition ") {
            rest.push_str(&dsl[at..line_end]);
            at = line_end;
            continue;
        }
        let open = dsl[at..].find('{').map(|o| at + o).ok_or("a condition needs its `{ expression }`")?;
        let close = matching(dsl, open).ok_or("a condition's `{` needs its `}`")?;
        let header = dsl[at..open].trim().trim_start_matches("condition").trim();
        let (name, params) = header.split_once('(').ok_or_else(|| format!("condition `{header}`: needs `name(parameter: type, ...)`"))?;
        let params = params.trim().strip_suffix(')').ok_or_else(|| format!("condition {name}: its parameters need their `)`"))?;
        let mut parameters = Map::new();
        for param in split_top(params) {
            let (p, t) = param.split_once(':').ok_or_else(|| format!("condition {name}: parameter `{param}` needs `name: type`"))?;
            parameters.insert(p.trim().to_owned(), param_type(t.trim()).map_err(|e| format!("condition {name}: {e}"))?);
        }
        let name = name.trim().to_owned();
        let expression = dsl[open + 1..close].trim().to_owned();
        conditions.insert(name.clone(), json!({"name": name, "expression": expression, "parameters": parameters}));
        rest.extend(std::iter::repeat_n('\n', dsl[at..=close].matches('\n').count()));
        at = close + 1;
    }
    Ok((rest, conditions))
}

/// The `}` closing the `{` at `open`.
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

/// `a: string, b: map<list<int>>` split at its top-level commas.
fn split_top(params: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0, 0);
    for (i, c) in params.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(params[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(params[start..].trim());
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// A condition parameter's type (`string`, `list<string>`, `map<int>`) as the API names it.
fn param_type(t: &str) -> Result<Value, String> {
    for (generic, name) in [("list<", "TYPE_NAME_LIST"), ("map<", "TYPE_NAME_MAP")] {
        if let Some(inner) = t.strip_prefix(generic).and_then(|i| i.strip_suffix('>')) {
            return Ok(json!({"type_name": name, "generic_types": [param_type(inner.trim())?]}));
        }
    }
    let name = match t {
        "any" => "TYPE_NAME_ANY",
        "bool" => "TYPE_NAME_BOOL",
        "string" => "TYPE_NAME_STRING",
        "int" => "TYPE_NAME_INT",
        "uint" => "TYPE_NAME_UINT",
        "double" => "TYPE_NAME_DOUBLE",
        "duration" => "TYPE_NAME_DURATION",
        "timestamp" => "TYPE_NAME_TIMESTAMP",
        "ipaddress" => "TYPE_NAME_IPADDRESS",
        other => return Err(format!("unknown parameter type `{other}`")),
    };
    Ok(json!({"type_name": name}))
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

/// `user`, `user:*` (anyone of the type) or `team#member` (a set of subjects), each possibly
/// `with` a condition.
fn directly_related(item: &str) -> Result<Value, String> {
    let (item, condition) = match item.split_once(" with ") {
        Some((i, c)) if !c.trim().is_empty() => (i.trim(), Some(c.trim())),
        Some(_) => return Err(format!("`{item}`: `with` needs a condition's name")),
        None => (item, None),
    };
    let mut related = if let Some(t) = item.strip_suffix(":*") {
        json!({"type": t, "wildcard": {}})
    } else if let Some((t, relation)) = item.split_once('#') {
        json!({"type": t, "relation": relation})
    } else {
        json!({"type": item})
    };
    if let Some(c) = condition {
        related["condition"] = json!(c);
    }
    Ok(related)
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

    // the API's form of conditions (as `fga model transform` gives it), and a subject with one
    #[test]
    fn conditions_convert_with_their_parameters() {
        let dsl = "model\n  schema 1.1\ntype user\ntype doc\n  relations\n    define viewer: [user, user with in_region, user:* with in_region]\n\
                   condition in_region(region: string, allowed: list<string>, limits: map<list<int>>) {\n  region in allowed\n}\n";
        let model = to_json(dsl).unwrap();
        assert_eq!(
            model["conditions"],
            json!({"in_region": {"name": "in_region", "expression": "region in allowed", "parameters": {
                "region": {"type_name": "TYPE_NAME_STRING"},
                "allowed": {"type_name": "TYPE_NAME_LIST", "generic_types": [{"type_name": "TYPE_NAME_STRING"}]},
                "limits": {"type_name": "TYPE_NAME_MAP", "generic_types": [{"type_name": "TYPE_NAME_LIST", "generic_types": [{"type_name": "TYPE_NAME_INT"}]}]}}}})
        );
        assert_eq!(
            model["type_definitions"][1]["metadata"]["relations"]["viewer"]["directly_related_user_types"],
            json!([{"type": "user"}, {"type": "user", "condition": "in_region"}, {"type": "user", "wildcard": {}, "condition": "in_region"}])
        );
    }

    #[test]
    fn modules_mixed_operators_and_stray_text_are_refused() {
        let with = |def: &str| to_json(&format!("model\n  schema 1.1\ntype doc\n  relations\n    define {def}\n"));
        assert!(with("viewer: a or b and c").unwrap_err().contains("need parentheses"));
        assert!(with("viewer: (a or b").unwrap_err().contains("ends too soon"));
        assert!(to_json("model\n  schema 1.1\nmodule docs").unwrap_err().contains("not supported"));
        assert!(to_json("model\n  schema 1.1\ncondition c(x: money) { x > 1 }").unwrap_err().contains("unknown parameter type `money`"));
        assert!(to_json("type doc").unwrap_err().contains("no `schema`"));
    }
}
