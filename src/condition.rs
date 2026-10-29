//! A plan's condition (the sidecar's `plan`: which resources a subject may access, as a condition
//! on their attributes) evaluated against one row, as the engine (Cerbos, CEL) evaluates it, and
//! as rewrite/compile.rs compiles it: equality is defined for every pair of values (null equals only null,
//! values of different kinds are unequal), an ordering of a null or of different kinds is an error,
//! which `and` and `or` absorb as SQL absorbs unknown, and a row is allowed only when the condition
//! is true.
//!
//! The language: `{"op": "and|or|not|eq|ne|lt|le|gt|ge|in", "args": [...]}`, an argument being
//! `{"column": name}` (the row's attribute), `{"id": true}` (the row's id), `{"value": json}` or
//! an expression. Anything else is unknown: denied.

use serde_json::Value;
use std::cmp::Ordering;

/// Whether the condition allows a row with this id and these attributes.
pub fn allows(condition: &Value, id: &str, attrs: &Value) -> bool {
    eval(condition, id, attrs) == Some(true)
}

fn eval(e: &Value, id: &str, attrs: &Value) -> Option<bool> {
    let op = e.get("op")?.as_str()?;
    let args = e.get("args")?.as_array()?;
    let all = |want: bool| -> Option<bool> {
        // and: any false is false; or: any true is true; otherwise unknown unless all are known
        let mut unknown = false;
        for a in args {
            match eval(a, id, attrs) {
                Some(b) if b != want => return Some(b),
                None => unknown = true,
                _ => {}
            }
        }
        (!unknown).then_some(want)
    };
    match (op, args.as_slice()) {
        ("and", _) => all(true),
        ("or", _) => all(false),
        ("not", [a]) => eval(a, id, attrs).map(|b| !b),
        ("in", [a, list]) => {
            let a = operand(a, id, attrs);
            Some(operand(list, id, attrs)?.as_array()?.iter().any(|item| equal(a.as_ref(), item)))
        }
        ("eq", [a, b]) => Some(equal(operand(a, id, attrs).as_ref(), operand(b, id, attrs).as_deref().unwrap_or(&Value::Null))),
        ("ne", [a, b]) => Some(!equal(operand(a, id, attrs).as_ref(), operand(b, id, attrs).as_deref().unwrap_or(&Value::Null))),
        (_, [a, b]) => {
            let ordering = compare(operand(a, id, attrs)?.as_ref(), operand(b, id, attrs)?.as_ref())?;
            match op {
                "lt" => Some(ordering == Ordering::Less),
                "le" => Some(ordering != Ordering::Greater),
                "gt" => Some(ordering == Ordering::Greater),
                "ge" => Some(ordering != Ordering::Less),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A condition's values, in order, and its skeleton: the condition with each value replaced by
/// `{"param": n, "kind": k}` (k: "string", "number", "bool", or "list" with "of"). Two subjects
/// whose plans have the same skeleton are answered by one compiled filter, its values read at
/// execution (rewrite/compile.rs).
pub fn parameterized(condition: &Value) -> (Value, Vec<Value>) {
    let mut values = Vec::new();
    let skeleton = skeleton(condition, &mut values);
    (skeleton, values)
}

fn skeleton(e: &Value, values: &mut Vec<Value>) -> Value {
    match e {
        Value::Object(map) if map.contains_key("value") => {
            let v = &map["value"];
            let kind = match v {
                Value::Array(items) => {
                    let of = items.first().map_or("string", kind_of);
                    return push(values, v.clone(), serde_json::json!({"kind": "list", "of": of}));
                }
                v => kind_of(v),
            };
            push(values, v.clone(), serde_json::json!({"kind": kind}))
        }
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), skeleton(v, values))).collect()),
        Value::Array(items) => Value::Array(items.iter().map(|v| skeleton(v, values)).collect()),
        other => other.clone(),
    }
}

fn push(values: &mut Vec<Value>, value: Value, mut placeholder: Value) -> Value {
    placeholder["param"] = values.len().into();
    values.push(value);
    placeholder
}

/// A JSON value's kind, as a row's attributes have them (jsonb_build_object of its columns).
pub fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        Value::Null => "null",
        _ => "string",
    }
}

/// A value as text, as its column's type reads it: a string as is, anything else as JSON.
pub fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// An argument's value; None when it is null or missing (unknown).
fn operand<'a>(arg: &'a Value, id: &'a str, attrs: &'a Value) -> Option<std::borrow::Cow<'a, Value>> {
    use std::borrow::Cow;
    let v = if let Some(column) = arg.get("column").and_then(Value::as_str) {
        Cow::Borrowed(attrs.get(column)?)
    } else if arg.get("id").is_some() {
        Cow::Owned(Value::String(id.to_owned()))
    } else {
        Cow::Borrowed(arg.get("value")?)
    };
    (!v.is_null()).then_some(v)
}

/// Equality as CEL has it: defined for every pair, null equal only to null (a missing attribute is
/// null here), values of different kinds unequal.
fn equal(a: Option<&std::borrow::Cow<Value>>, b: &Value) -> bool {
    match a {
        None => b.is_null(),
        Some(a) => compare(a, b) == Some(Ordering::Equal),
    }
}

/// Values of one kind ordered; values of different kinds, or null, have no order (an error in
/// CEL, unknown here).
fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64()?.partial_cmp(&y.as_f64()?),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn conditions_follow_sqls_three_valued_logic() {
        let mine_or_published = json!({"op": "or", "args": [
            {"op": "eq", "args": [{"column": "owner"}, {"value": "anne"}]},
            {"op": "eq", "args": [{"column": "status"}, {"value": "published"}]}]});
        assert!(allows(&mine_or_published, "1", &json!({"owner": "anne", "status": "draft"})));
        assert!(allows(&mine_or_published, "1", &json!({"owner": "bob", "status": "published"})));
        assert!(!allows(&mine_or_published, "1", &json!({"owner": "bob", "status": "draft"})));
        // equality is defined for a null owner: not bob's, so allowed, as CEL has it
        let not_bobs = json!({"op": "not", "args": [{"op": "eq", "args": [{"column": "owner"}, {"value": "bob"}]}]});
        assert!(allows(&not_bobs, "1", &json!({"owner": "anne"})));
        assert!(allows(&not_bobs, "1", &json!({"owner": null})));
        assert!(!allows(&not_bobs, "1", &json!({"owner": "bob"})));
        let not_draft = json!({"op": "ne", "args": [{"column": "status"}, {"value": "draft"}]});
        assert!(allows(&not_draft, "1", &json!({"status": null})) && !allows(&not_draft, "1", &json!({"status": "draft"})));
        // an ordering of a null is unknown, even negated
        let not_small = json!({"op": "not", "args": [{"op": "lt", "args": [{"column": "size"}, {"value": 10}]}]});
        assert!(!allows(&not_small, "1", &json!({"size": null})) && allows(&not_small, "1", &json!({"size": 10})));
        // or with an unknown arm is true when another arm is
        assert!(allows(&mine_or_published, "1", &json!({"owner": null, "status": "published"})));
        // numbers, lists, the id; kinds never mix
        let small = json!({"op": "lt", "args": [{"column": "size"}, {"value": 10}]});
        assert!(
            allows(&small, "1", &json!({"size": 9.5}))
                && !allows(&small, "1", &json!({"size": 10}))
                && !allows(&small, "1", &json!({"size": "9"}))
        );
        // different kinds are unequal, not unknown
        let nine = json!({"op": "ne", "args": [{"column": "size"}, {"value": 9}]});
        assert!(allows(&nine, "1", &json!({"size": "9"})));
        let listed = json!({"op": "in", "args": [{"id": true}, {"value": ["1", "2"]}]});
        assert!(allows(&listed, "2", &json!({})) && !allows(&listed, "3", &json!({})));
        assert!(!allows(&json!({"op": "regex", "args": []}), "1", &json!({})));
    }

    #[test]
    fn plans_of_one_shape_share_a_skeleton() {
        let anne = json!({"op": "or", "args": [
            {"op": "eq", "args": [{"column": "owner"}, {"value": "anne"}]},
            {"op": "in", "args": [{"column": "size"}, {"value": [1, 2]}]}]});
        let bob = json!({"op": "or", "args": [
            {"op": "eq", "args": [{"column": "owner"}, {"value": "bob"}]},
            {"op": "in", "args": [{"column": "size"}, {"value": [3]}]}]});
        let (a, values) = parameterized(&anne);
        assert_eq!(a, parameterized(&bob).0);
        assert_eq!(values, [json!("anne"), json!([1, 2])]);
        assert_eq!(a["args"][0]["args"][1], json!({"param": 0, "kind": "string"}));
        assert_eq!(a["args"][1]["args"][1], json!({"param": 1, "kind": "list", "of": "number"}));
        // another kind of value is another shape
        let numeric = json!({"op": "eq", "args": [{"column": "owner"}, {"value": 7}]});
        assert_ne!(parameterized(&numeric).0, parameterized(&json!({"op": "eq", "args": [{"column": "owner"}, {"value": "7"}]})).0);
    }
}
