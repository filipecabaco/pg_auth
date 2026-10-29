//! Attribute rules: a plan's condition compiled into a filter on the row's columns
//! (docs/proposals/cerbos.md).

use super::REPLAN_ON_BREADTH;
use super::guard::{Guard, call_sublink, hash_by_value, put};
use crate::pg::{Memory, Node, Oid, catalog, pg_sys};
use crate::sidecar;
use std::collections::HashMap;

pub const COMPILED: &str = "compiled condition";

/// Replace an attribute guard (`authz.permits_resource`) with the subject's plan compiled into a
/// filter on the row's columns, which the planner can use (an index on `owner_id` for
/// `owner_id = ...`). No value of the plan is a constant: each is read at execution by an
/// InitPlan (`authz.plan_value`), and a guard (`authz.plan_shape_is`) refuses a subject whose
/// plan has another shape; such a subject's cached plans are rebuilt first (`shapes_changed`).
/// Err: why the row-by-row policy stays.
pub fn compile_resource(g: &Guard) -> Result<String, String> {
    if !REPLAN_ON_BREADTH.get() {
        return Err("pg_auth.replan_on_breadth is off: a compiled filter is never reused by another shape of plan; rows are checked".into());
    }
    let p = &g.policy;
    let (ty, perm) = (&p.object_type, &p.permission);
    let shape = sidecar::current_shape(ty, perm).ok_or("the engine's plan could not be had: rows are checked, and fail closed")?;
    let plan: serde_json::Value = serde_json::from_str(&shape).map_err(|e| e.to_string())?;
    let columns = p.attrs.and_then(attribute_columns).ok_or("the policy's attributes are not plain name/column pairs")?;
    let m = g.query.memory();
    let c = Compiler { m, g, columns };
    let (filter, reason) = match plan["plan"].as_str() {
        Some("always") => (m.bool_const(true, false), format!("{ty}#{perm} allows every row")),
        Some("never") => (m.bool_const(false, false), format!("{ty}#{perm} allows no row: no rows read")),
        Some("condition") => {
            let filter = c
                .compile(&plan["condition"], false)
                .ok_or("the plan's condition does not compile against these columns' types: rows are checked")?;
            (filter, format!("{ty}#{perm}'s condition on the row's attributes: indexable filter"))
        }
        Some("none") => return Err("no subject: nothing is allowed".into()),
        _ => return Err(format!("{ty}#{perm}'s condition is outside what pg_auth compiles: rows are checked with the engine")),
    };
    let text = |s: &str| m.text_const(s).ok_or_else(|| format!("{s:?} holds a NUL"));
    let guard = call_sublink(m, "plan_shape_is", &[text(ty)?, text(perm)?, text(&shape)?]).ok_or("authz.plan_shape_is is missing")?;
    put(g, m.bool_expr(pg_sys::BoolExprType::AND_EXPR, m.list([guard, filter])));
    sidecar::note_planned_shape(ty, perm, &shape);
    Ok(reason)
}

/// `jsonb_build_object('name', column, ...)`'s pairs: each attribute's expression over the row.
fn attribute_columns(attrs: Node<'_>) -> Option<HashMap<String, Node<'_>>> {
    let args: Vec<Node> = attrs.cast::<pg_sys::FuncExpr>()?.args().iter().collect();
    args.chunks(2).map(|pair| Some((const_name(*pair.first()?)?, *pair.get(1)?))).collect()
}

/// A name literal: `text`, or `unknown` (a C string) where the parameter takes "any".
fn const_name(node: Node<'_>) -> Option<String> {
    let c = node.cast::<pg_sys::Const>()?;
    catalog::unknown_value(c).or_else(|| catalog::text_value(c))
}

struct Compiler<'g, 'a> {
    m: Memory<'a>,
    g: &'g Guard<'a>,
    columns: HashMap<String, Node<'a>>,
}

impl<'a> Compiler<'_, 'a> {
    /// A condition (its skeleton: values are `{"param": n, "kind": k}`) as a filter over the row's
    /// columns, evaluating as condition.rs does (CEL's equality, SQL's unknown for an ordering of
    /// a null). Equality stays `=` (indexable) where a null's unknown and CEL's false both deny
    /// the row; under an odd number of `not` (`negated`) it is `coalesce(col = v, false)`, and
    /// `ne` is `IS DISTINCT FROM`. None when it does not compile.
    fn compile(&self, e: &serde_json::Value, negated: bool) -> Option<Node<'a>> {
        let m = self.m;
        let op = e["op"].as_str()?;
        let args = e["args"].as_array()?;
        let boolean = |kind, negated| -> Option<Node<'a>> {
            let compiled = args.iter().map(|a| self.compile(a, negated)).collect::<Option<Vec<_>>>()?;
            Some(m.bool_expr(kind, m.list(compiled)))
        };
        match op {
            "and" => boolean(pg_sys::BoolExprType::AND_EXPR, negated),
            "or" => boolean(pg_sys::BoolExprType::OR_EXPR, negated),
            "not" if args.len() == 1 => boolean(pg_sys::BoolExprType::NOT_EXPR, !negated),
            "in" if args.len() == 2 => {
                let column = self.row_expr(&args[0])?;
                let list = &args[1];
                let typ = catalog::expr_type(column);
                if list["kind"].as_str() != Some("list") || list["of"].as_str() != Some(kind_of_type(typ)) {
                    // no value of another kind equals the column's
                    return Some(m.bool_const(false, false));
                }
                let values = self.plan_values(list["param"].as_i64()?, typ)?;
                let opno = catalog::operator("=", typ)?;
                let any = m.any(opno, column, values, collation_of(column), hash_by_value(opno, typ));
                Some(definite(m, any, negated))
            }
            "eq" | "ne" | "lt" | "le" | "gt" | "ge" if args.len() == 2 => {
                // the row's side on the left: a value on the left turns the comparison around
                let (row, param, op) = match (self.row_expr(&args[0]), self.row_expr(&args[1])) {
                    (Some(r), None) => (r, &args[1], op),
                    (None, Some(r)) => (r, &args[0], mirror(op)),
                    _ => return None,
                };
                let typ = catalog::expr_type(row);
                if param["kind"].as_str() != Some(kind_of_type(typ)) {
                    // values of different kinds: unequal, and without order
                    return Some(match op {
                        "eq" => m.bool_const(false, false),
                        "ne" => m.bool_const(true, false),
                        _ => m.bool_const(false, true),
                    });
                }
                let value = self.plan_value(param["param"].as_i64()?, typ)?;
                // strings order as Cerbos orders them, by code point; equality keeps the column's
                // collation, which its index has
                let collation = collation_of(row);
                let collation =
                    if collation != pg_sys::InvalidOid && !matches!(op, "eq" | "ne") { pg_sys::C_COLLATION_OID } else { collation };
                if op == "ne" {
                    // a null differs from every value, as CEL has it
                    return Some(m.operator(catalog::operator("=", typ)?, row, value, collation, true));
                }
                let sql_op = match op {
                    "eq" => "=",
                    "lt" => "<",
                    "le" => "<=",
                    "gt" => ">",
                    _ => ">=",
                };
                let clause = m.operator(catalog::operator(sql_op, typ)?, row, value, collation, false);
                Some(if op == "eq" { definite(m, clause, negated) } else { clause })
            }
            _ => None,
        }
    }

    /// The row's side of a comparison: an attribute's column, or the row's id; None for a value.
    /// The id is compared as text, as the policy sends it and the engine has it (a resource id
    /// is a string): `2` is not the id `"02"`.
    fn row_expr(&self, arg: &serde_json::Value) -> Option<Node<'a>> {
        if let Some(name) = arg["column"].as_str() {
            return Some(self.m.copy(*self.columns.get(name)?));
        }
        if arg.get("id").is_some() {
            let key = self.m.copy(self.g.policy.key);
            return self.m.coerce(key, catalog::expr_type(key), pg_sys::TEXTOID);
        }
        None
    }

    /// `(SELECT authz.plan_value(type, permission, n, typ))::typ`: the `n`th value of whoever runs
    /// it.
    fn plan_value(&self, n: i64, typ: Oid) -> Option<Node<'a>> {
        let value = self.plan_call("plan_value", n, typ)?;
        self.m.coerce(value, pg_sys::TEXTOID, typ)
    }

    /// `(SELECT authz.plan_values(type, permission, n, typ))::typ[]`.
    fn plan_values(&self, n: i64, typ: Oid) -> Option<Node<'a>> {
        let array = catalog::array_type(typ)?;
        let values = self.plan_call("plan_values", n, typ)?;
        self.m.coerce(values, pg_sys::TEXTARRAYOID, array)
    }

    fn plan_call(&self, func: &str, n: i64, typ: Oid) -> Option<Node<'a>> {
        let m = self.m;
        let p = &self.g.policy;
        let args = [m.text_const(&p.object_type)?, m.text_const(&p.permission)?, m.int4_const(i32::try_from(n).ok()?), m.oid_const(typ)];
        call_sublink(m, func, &args)
    }
}

fn mirror(op: &str) -> &str {
    match op {
        "lt" => "gt",
        "le" => "ge",
        "gt" => "lt",
        "ge" => "le",
        other => other,
    }
}

/// The kind a column's values have as a row's attributes (jsonb_build_object): numbers, booleans,
/// and everything else as a string.
fn kind_of_type(typ: Oid) -> &'static str {
    match typ {
        pg_sys::INT2OID | pg_sys::INT4OID | pg_sys::INT8OID | pg_sys::FLOAT4OID | pg_sys::FLOAT8OID | pg_sys::NUMERICOID => "number",
        pg_sys::BOOLOID => "bool",
        _ => "string",
    }
}

/// An equality as CEL has it where it is negated: false rather than unknown for a null.
fn definite<'a>(m: Memory<'a>, clause: Node<'a>, negated: bool) -> Node<'a> {
    if negated { m.coalesce_bool(clause, false) } else { clause }
}

/// The collation comparisons on `node` use: its own, or the default for a collatable type that
/// has none (the id cast to text, built without the parser's collation pass).
fn collation_of(node: Node<'_>) -> Oid {
    let collation = catalog::expr_collation(node);
    if collation == pg_sys::InvalidOid && catalog::type_collation(catalog::expr_type(node)) != pg_sys::InvalidOid {
        pg_sys::DEFAULT_COLLATION_OID
    } else {
        collation
    }
}
