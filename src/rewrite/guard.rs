//! Finding pg_auth's policies in a query and replacing them, at planning, with what the engine's
//! answer allows: the listed-ids filter, or (attribute rules) a compiled condition (compile.rs).

use super::{BATCH_CHECK, BATCHED, LISTED, PER_ROW, PLAN_LOOKUP, record, record_advised};
use crate::pg::{Node, Oid, Ptr, catalog, pg_sys};
use crate::sidecar::{self, Listed};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// A pg_auth policy predicate: `authz.permits('<type>', '<permission>', key[::text])`, or
/// `authz.permits_resource(..., attributes)`, or (after planning) the listed-ids filter it became.
pub struct Policy<'a> {
    /// the row's object id: a column (a Var), or a key expression over the row's columns
    pub key: Node<'a>,
    pub object_type: String,
    pub permission: String,
    /// the listed filter is `col = (SELECT one id)` rather than `col = ANY(...)`
    pub single: bool,
    /// `authz.permits_resource`'s attributes (`jsonb_build_object('name', column, ...)`): a table
    /// protected by its rows' attributes, for an engine that decides on them
    pub attrs: Option<Node<'a>>,
}

impl Policy<'_> {
    /// The protected column's number, or 0 for a key expression.
    pub fn column(&self) -> i16 {
        self.key.cast::<pg_sys::Var>().map_or(0, Ptr::varattno)
    }

    /// The range table index of the relation whose row the key reads.
    pub fn varno(&self) -> u32 {
        key_varno(self.key).unwrap_or(0)
    }
}

/// Where a policy sits in its query, which `put` replaces.
#[derive(Clone, Copy)]
pub enum Place<'a> {
    /// the relation's `i`th security qual
    Qual(usize),
    /// the `i`th arm of the OR Postgres builds from a table's permissive policies (a table
    /// protected several ways), in a security qual or a written row's check
    OrArm(Ptr<'a, pg_sys::BoolExpr>, usize),
    /// a write's check on the new row (WITH CHECK)
    Check(Ptr<'a, pg_sys::WithCheckOption>),
}

/// A policy on one relation of a query, to be rewritten.
pub struct Guard<'a> {
    pub policy: Policy<'a>,
    pub query: Ptr<'a, pg_sys::Query>,
    pub rte: Ptr<'a, pg_sys::RangeTblEntry>,
    pub place: Place<'a>,
}

/// The one relation of this query level a key reads (None: no column, or several relations).
fn key_varno(key: Node<'_>) -> Option<u32> {
    let vars = catalog::vars(key);
    let first = vars.first()?;
    let same = vars.iter().all(|v| v.varno() == first.varno() && v.varlevelsup() == 0);
    if same { u32::try_from(first.varno()).ok() } else { None }
}

/// A key pg_auth can plan for: a column of this query level, or an expression over one
/// relation's columns that is neither volatile nor a sub-select (a key expression's value for a
/// row never changes, which the listed filter and an index on it rely on).
pub fn usable_key(key: Node<'_>) -> bool {
    if let Some(var) = key.cast::<pg_sys::Var>() {
        return var.varlevelsup() == 0;
    }
    key_varno(key).is_some() && !catalog::is_volatile(key) && !catalog::has_sublink(key)
}

thread_local! {
    /// The `authz` schema found to be pg_auth's, by its oid: checked once, not at every planning.
    static OURS: std::cell::Cell<Oid> = const { std::cell::Cell::new(pg_sys::InvalidOid) };
}

/// The authz schema, when it is pg_auth's: a schema of that name in a database without the
/// extension (a user's own, with a permits of its own) is never mistaken for it.
pub fn authz_namespace() -> Option<Oid> {
    let nsp = catalog::namespace(c"authz")?;
    if OURS.with(std::cell::Cell::get) == nsp {
        return Some(nsp);
    }
    let text = [pg_sys::TEXTOID; 3];
    let permits = catalog::function("authz", "permits", &text)?;
    catalog::function_of_extension(permits, c"pg_auth").then(|| {
        OURS.with(|o| o.set(nsp));
        nsp
    })
}

/// Shape every pg_auth policy of the query (and its subqueries) with the engine's answer.
pub fn shape(parse: Ptr<'_, pg_sys::Query>) {
    let Some(nsp) = authz_namespace() else { return };
    let mut guards = Vec::new();
    collect(parse, nsp, &mut guards);
    if guards.is_empty() {
        return;
    }

    let subject = crate::subject::current_as_caller();
    // where ids are not listed, plain reads check rows in batches (AuthzBatch), the rest one by one
    let checked = if BATCH_CHECK.get() && read_only(parse) { BATCHED } else { PER_ROW };
    // a write's target often carries the same check twice (its command's policy and SELECT's,
    // with one permission for both): security quals are ANDed, so the copy is `true`
    let mut seen = HashSet::new();
    let guards: Vec<Guard> = guards
        .into_iter()
        .filter(|g| {
            let Place::Qual(i) = g.place else { return true };
            let p = &g.policy;
            let mut key = catalog::node_text(p.key);
            if let Some(attrs) = p.attrs {
                key.push_str(&catalog::node_text(attrs));
            }
            if seen.insert((g.rte.addr(), p.object_type.clone(), p.permission.clone(), key)) {
                return true;
            }
            g.rte.security_quals().set(i, g.rte.memory().bool_const(true, false));
            false
        })
        .collect();
    for g in guards {
        let p = &g.policy;
        if !PLAN_LOOKUP.get() {
            record(g.rte, checked, "pg_auth.plan_lookup is off".into());
            continue;
        }
        if p.attrs.is_some() {
            match super::compile::compile_resource(&g) {
                Ok(reason) => record(g.rte, super::compile::COMPILED, reason),
                Err(reason) => record(g.rte, PER_ROW, reason),
            }
            continue;
        }
        let Some(subject) = &subject else {
            sidecar::note_planned(None, &p.object_type, &p.permission, None);
            // no subject: nothing is allowed, which the listed form answers without reading rows
            if replace_with_listed(&g, false) {
                record(g.rte, LISTED, "no subject: nothing allowed, no rows read".into());
            }
            continue;
        };
        let listed = sidecar::list(subject, &p.object_type, &p.permission);
        if let Ok(l) = &listed {
            sidecar::note_planned(Some(subject), &p.object_type, &p.permission, Some(l));
        }
        match listed.as_deref() {
            Ok(Listed::Ids(ids)) => {
                let n = ids.len();
                // at most one id: `col = (SELECT ...)`, which keeps the order of an index on
                // (col, ...) that `col = ANY(...)` loses; AuthzBatch (plain reads) holds an
                // ordered fallback for a subject with more ids reusing the plan
                let single =
                    n <= 1 && checked == BATCHED && !matches!(g.place, Place::OrArm(..)) && ordered_page(&g) && !compared_in_query(&g);
                if replace_with_listed(&g, single) {
                    let reason = if n == 0 {
                        format!("{subject} may access no {} objects: no rows read", p.object_type)
                    } else {
                        format!("{subject} may access {n} {} objects: indexable filter", p.object_type)
                    };
                    record(g.rte, LISTED, reason);
                } else {
                    record(g.rte, checked, "no indexable form for the id column's type".into());
                }
            }
            // an UPDATE or DELETE that may reach many rows, by a subject whose permission index set
            // is fresh: its rows found through that set (an index on the column), not each checked
            Ok(Listed::TooMany)
                if bulk_write_target(&g)
                    && matches!(sidecar::index_status(subject, &p.object_type, &p.permission), sidecar::IndexStatus::Fresh(n) if n <= WRITE_LIST_LIMIT)
                    && replace_with_listed(&g, false) =>
            {
                record(
                    g.rte,
                    LISTED,
                    format!(
                        "{subject} may access more {} objects than the engine lists: this write lists them from its permission index set, indexable filter",
                        p.object_type
                    ),
                );
            }
            Ok(Listed::TooMany) => {
                let advice = match sidecar::index_status(subject, &p.object_type, &p.permission) {
                    sidecar::IndexStatus::Fresh(n) => format!("answered from the permission index ({n} objects), without engine calls"),
                    sidecar::IndexStatus::Missing => {
                        "no fresh permission index set yet: rows are checked with the engine until the sidecar fills it".into()
                    }
                    sidecar::IndexStatus::Off => {
                        "rows are checked with the engine: turn pg_auth.permission_index on to answer them from Postgres".into()
                    }
                };
                record_advised(
                    g.rte,
                    checked,
                    format!("{subject} may access more {} objects than the engine lists: rows are checked", p.object_type),
                    Some(advice),
                );
            }
            Err(e) => record(g.rte, checked, format!("{e}; rows are checked and fail closed")),
        }
    }
}

/// Plain reads only: writes and row locks need EvalPlanQual rechecks, which AuthzBatch does not do.
pub fn read_only(parse: Ptr<'_, pg_sys::Query>) -> bool {
    parse.command_type() == pg_sys::CmdType::CMD_SELECT && parse.row_marks().is_empty()
}

/// Objects a fresh permission index set may have for a write's rows to be found through it.
const WRITE_LIST_LIMIT: usize = 10_000;

/// The rows an UPDATE or DELETE changes, when its own conditions do not pin one row by a unique
/// key: a write that may reach many rows, where authz.permits on each costs more than listing the
/// subject's index set once (a point write keeps the check: one call, no list to build).
fn bulk_write_target(g: &Guard) -> bool {
    let q = g.query;
    if matches!(g.place, Place::Check(_)) || !matches!(q.command_type(), pg_sys::CmdType::CMD_UPDATE | pg_sys::CmdType::CMD_DELETE) {
        return false;
    }
    let Some(rtindex) = q.rtable().position(g.rte).and_then(|i| i32::try_from(i + 1).ok()) else { return false };
    rtindex == q.result_relation() && !pins_unique_key(q, rtindex, g.rte.relid())
}

/// Does the query's WHERE fix a single-column unique key of relation `rtindex` to one value?
fn pins_unique_key(q: Ptr<'_, pg_sys::Query>, rtindex: i32, relid: Oid) -> bool {
    let Some(quals) = q.jointree().and_then(Ptr::quals) else { return false };
    let conjuncts: Vec<Node> = match quals.cast::<pg_sys::BoolExpr>() {
        Some(and) if and.boolop() == pg_sys::BoolExprType::AND_EXPR => and.args().iter().collect(),
        _ => vec![quals],
    };
    let keys = catalog::unique_columns(relid);
    conjuncts.into_iter().any(|c| {
        let Some(op) = c.cast::<pg_sys::OpExpr>() else { return false };
        let (Some(a), Some(b)) = (op.args().get(0), op.args().get(1)) else { return false };
        let pinned = |var: Node, other: Node| {
            var.cast::<pg_sys::Var>().is_some_and(|v| v.varno() == rtindex && v.varlevelsup() == 0 && keys.contains(&v.varattno()))
                && !catalog::reads_this_level(other)
        };
        catalog::operator_name(op.opno()).as_deref() == Some("=") && (pinned(a, b) || pinned(b, a))
    })
}

/// Every pg_auth policy in the query and its subqueries (FROM, CTEs, and sub-selects anywhere in
/// its expressions: `IN (SELECT ...)`, `EXISTS (...)`, other policies): each security qual of a
/// relation (a write carries one per command, e.g. UPDATE's and SELECT's), and each check on a
/// written row (WITH CHECK).
fn collect<'a>(query: Ptr<'a, pg_sys::Query>, nsp: Oid, out: &mut Vec<Guard<'a>>) {
    for rte in query.rtable().iter() {
        match rte.rtekind() {
            pg_sys::RTEKind::RTE_RELATION => {
                for (i, qual) in rte.security_quals().iter().enumerate() {
                    if let Some(policy) = match_policy(qual, nsp) {
                        out.push(Guard { policy, query, rte, place: Place::Qual(i) });
                    } else {
                        out.extend(or_arms(qual, nsp).into_iter().map(|(policy, place)| Guard { policy, query, rte, place }));
                    }
                }
            }
            pg_sys::RTEKind::RTE_SUBQUERY => {
                if let Some(sub) = rte.subquery() {
                    collect(sub, nsp, out);
                }
            }
            _ => {}
        }
    }
    let target = usize::try_from(query.result_relation()).ok().filter(|r| *r > 0).and_then(|r| query.rtable().get(r - 1));
    if let Some(rte) = target {
        for wco in query.with_check_options().iter() {
            let Some(qual) = wco.qual() else { continue };
            match match_policy(qual, nsp) {
                Some(policy) => out.push(Guard { policy, query, rte, place: Place::Check(wco) }),
                None => out.extend(or_arms(qual, nsp).into_iter().map(|(policy, place)| Guard { policy, query, rte, place })),
            }
        }
    }
    for cte in query.cte_list().iter() {
        if let Some(q) = cte.cte_query().and_then(Node::cast::<pg_sys::Query>) {
            collect(q, nsp, out);
        }
    }
    // sub-selects in expressions (FROM subqueries and CTEs are collected above)
    for sub in catalog::sublink_queries(query) {
        collect(sub, nsp, out);
    }
}

/// The pg_auth arms of an OR (Postgres ORs a table's permissive policies into one qual), each
/// with its place in it; the others (any other policy) stay as they are.
fn or_arms(qual: Node<'_>, nsp: Oid) -> Vec<(Policy<'_>, Place<'_>)> {
    let Some(or) = qual.cast::<pg_sys::BoolExpr>().filter(|b| b.boolop() == pg_sys::BoolExprType::OR_EXPR) else { return Vec::new() };
    or.args().iter().enumerate().filter_map(|(i, arm)| Some((match_policy(arm, nsp)?, Place::OrArm(or, i)))).collect()
}

/// Match `authz.permits(Const, Const, col[::text])`, or `authz.permits_resource(Const, Const,
/// col[::text], attributes)`.
pub fn match_policy(qual: Node<'_>, nsp: Oid) -> Option<Policy<'_>> {
    let func = qual.cast::<pg_sys::FuncExpr>()?;
    if catalog::function_namespace(func.funcid()) != nsp {
        return None;
    }
    let args = func.args();
    let attrs = match catalog::function_name(func.funcid())?.as_str() {
        "permits" => None,
        "permits_resource" => Some(args.get(3)?),
        _ => return None,
    };
    let object_type = const_text(args.get(0)?)?;
    let permission = const_text(args.get(1)?)?;
    let mut key = args.get(2)?;
    loop {
        key = if let Some(c) = key.cast::<pg_sys::CoerceViaIO>() {
            c.arg()?
        } else if let Some(r) = key.cast::<pg_sys::RelabelType>() {
            r.arg()?
        } else {
            break;
        };
    }
    usable_key(key).then_some(Policy { key, object_type, permission, single: false, attrs })
}

pub fn const_text(node: Node<'_>) -> Option<String> {
    catalog::text_value(node.cast::<pg_sys::Const>()?)
}

/// Whether the guard's query reads only its table, in an order: a page, where the one-id form's
/// ordered index scan pays off. Elsewhere (joins, aggregates) `col = ANY(...)` serves every
/// listed subject when a cached plan is reused, where a one-id plan would leave subjects with
/// more ids an unfiltered scan.
fn ordered_page(g: &Guard) -> bool {
    !g.query.sort_clause().is_empty() && g.query.rtable().len() == 1
}

/// Whether the query's own conditions or joins mention the protected column. Then the one-id
/// form's `col = $one` would join the column's equivalence class, and the planner would derive
/// `other = $one` on other relations (a join on the column) or a gating `$one = 5` (a condition
/// on it), outside AuthzBatch, where nothing answers for a subject with more ids reusing the plan.
fn compared_in_query(g: &Guard) -> bool {
    let column = g.policy.column();
    if column <= 0 {
        // a key expression: its columns' equivalences are not tracked here
        return true;
    }
    let Some(jointree) = g.query.jointree() else { return false };
    catalog::attnos([jointree.node()], g.policy.varno()).contains(&i32::from(column))
}

/// Replace the guard with `col = ANY ((SELECT authz.allowed_<type>(...)))`, or with `single`
/// `col = (SELECT authz.allowed_one_<type>(...))`. False when the key's type has no listed form.
pub fn replace_with_listed(g: &Guard, single: bool) -> bool {
    let p = &g.policy;
    let vartype = catalog::expr_type(p.key);
    let func = match vartype {
        pg_sys::INT4OID => "int4",
        pg_sys::INT8OID => "int8",
        pg_sys::UUIDOID => "uuid",
        pg_sys::TEXTOID => "text",
        _ => return false,
    };
    let func = if single { format!("allowed_one_{func}") } else { format!("allowed_{func}") };
    let Some(opno) = catalog::operator("=", vartype) else { return false };

    let relid = g.rte.relid();
    // a key expression is read from the binding by allowed_* when it must list values itself
    let col = if p.column() > 0 {
        let Some(name) = catalog::attribute_name(relid, p.column()) else { return false };
        name
    } else {
        String::new()
    };
    let m = g.query.memory();
    let Some(sub) = allowed_query(m, &func, &p.object_type, &p.permission, relid, &col) else { return false };
    let key = m.copy(p.key);
    let collation = catalog::expr_collation(p.key);
    let filter = if single {
        m.operator(opno, key, m.sublink(sub), collation, false)
    } else {
        // hashed, as Postgres hashes `= ANY` over a constant array (it does not for a Param): the
        // ids come from an InitPlan that takes no parameters, so they are the same for every row
        // of the query and the table is built once, where a scan would otherwise compare each row
        // with every id (by-value ids only: see hash_by_value)
        m.any(opno, key, m.sublink(sub), collation, hash_by_value(opno, vartype))
    };
    put(g, filter);
    true
}

/// A hash function for `= ANY` over an InitPlan's array, for elements passed by value only.
/// Postgres builds the hash table from the array it reads on the first row, keeping pointers into
/// it; it hashes only constant arrays, whose memory lives as long as the plan. An InitPlan's array
/// under 127 bytes is stored packed, and reading it unpacks a copy in the first row's memory,
/// which the next rows reuse: elements passed by reference (text, uuid) would then be compared
/// against what replaced them. Elements passed by value (integers) are copied into the table.
pub fn hash_by_value(opno: Oid, typ: Oid) -> Option<Oid> {
    catalog::hash_function(opno, typ)
}

/// `filter` where the guard's policy was: its arm of the OR of the table's policies, its security
/// qual, or the written row's check (which holds for the row exactly when the policy would).
pub fn put<'a>(g: &Guard<'a>, filter: Node<'a>) {
    match g.place {
        Place::OrArm(or, i) => {
            or.args().set(i, filter);
        }
        Place::Qual(i) => {
            g.rte.security_quals().set(i, filter);
        }
        Place::Check(wco) => wco.set_qual(Some(filter)),
    }
    // the planner only processes sub-selects in queries marked as having them
    g.query.set_has_sublinks(true);
}

/// `SELECT authz.<func>(object_type, permission, relid, col)`, analysed, built as nodes: the
/// query the parser would make of that text, without parsing it for every guard of every
/// statement. None when the function does not exist.
fn allowed_query<'a>(
    m: crate::pg::Memory<'a>,
    func: &str,
    object_type: &str,
    permission: &str,
    relid: Oid,
    col: &str,
) -> Option<Ptr<'a, pg_sys::Query>> {
    call_query(m, func, &[m.text_const(object_type)?, m.text_const(permission)?, m.oid_const(relid), m.text_const(col)?])
}

/// The generation the cache was filled in, and the functions found since.
type Functions = (u64, HashMap<(String, Vec<Oid>), (Oid, Oid)>);

thread_local! {
    /// authz functions the hook calls, by name and argument types: (oid, return type)
    static FUNCTIONS: RefCell<Functions> = RefCell::new((0, HashMap::new()));
}

/// `SELECT authz.<func>(args...)` as nodes; None when the function does not exist.
pub fn call_query<'a>(m: crate::pg::Memory<'a>, func: &str, args: &[Node<'a>]) -> Option<Ptr<'a, pg_sys::Query>> {
    let argtypes: Vec<Oid> = args.iter().map(|a| catalog::expr_type(*a)).collect();
    // looked up once per configuration generation (the extension created again bumps it)
    let generation = crate::ffi::config_generation();
    let key = (func.to_string(), argtypes);
    let known = FUNCTIONS.with(|f| {
        let f = f.borrow();
        (f.0 == generation).then(|| f.1.get(&key).copied()).flatten()
    });
    let (funcid, rettype) = if let Some(found) = known {
        found
    } else {
        let funcid = catalog::function("authz", func, &key.1)?;
        let found = (funcid, catalog::function_result_type(funcid));
        FUNCTIONS.with(|f| {
            let mut f = f.borrow_mut();
            if f.0 != generation {
                *f = (generation, HashMap::new());
            }
            f.1.insert(key, found);
        });
        found
    };
    m.select_call(funcid, rettype, m.list(args.iter().copied()), func)
}

/// `(SELECT authz.<func>(args...))`: an InitPlan once planned, evaluated once per execution.
pub fn call_sublink<'a>(m: crate::pg::Memory<'a>, func: &str, args: &[Node<'a>]) -> Option<Node<'a>> {
    Some(m.sublink(call_query(m, func, args)?))
}
