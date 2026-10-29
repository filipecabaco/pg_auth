//! What policies on protected tables call. Decisions come from the source-of-truth engine
//! through the sidecar (src/sidecar.rs); nothing is decided from data stored in Postgres.

use crate::sidecar::{self, Listed, Plan};
use crate::subject;
use pgrx::prelude::*;

fn fail(e: &str) -> ! {
    ereport!(ERROR, PgSqlErrorCode::ERRCODE_CONNECTION_FAILURE, format!("pg_auth: {e}"));
}

/// A counter as the `bigint` SQL has, saturating.
pub fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// May the current subject access object `id` of `object_type` with `permission`? The policy on
/// every protected table; also the row check used when a subject may access too many objects to
/// list. No subject: false. Engine unreachable: error (fail closed).
#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn permits(object_type: &str, permission: &str, id: Option<&str>) -> bool {
    let (Some(subject), Some(id)) = (subject::current(), id) else { return false };
    sidecar::permits(&subject, object_type, permission, id).unwrap_or_else(|e| fail(&e))
}

/// The policy on a table protected by its rows' attributes (authz.protect attributes =>), for an
/// engine that decides on them: the subject's plan, fetched once per transaction and evaluated
/// here against the row, with no round trip per row; a plan outside the condition language asks
/// the engine for each row. No subject: false. Engine unreachable: error (fail closed).
#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
#[expect(clippy::needless_pass_by_value, reason = "pgrx hands a JsonB argument over by value")]
fn permits_resource(object_type: &str, permission: &str, id: Option<&str>, attrs: pgrx::JsonB) -> bool {
    let (Some(subject), Some(id)) = (subject::current(), id) else { return false };
    let plan = sidecar::plan(&subject, object_type, permission, subject::principal_key(), subject::principal).unwrap_or_else(|e| fail(&e));
    match &*plan {
        Plan::Always => true,
        Plan::Never => false,
        Plan::Condition(condition) => crate::condition::allows(condition, id, &attrs.0),
        Plan::Rows => sidecar::check_resource(&subject, object_type, permission, id, &attrs.0, subject::principal().as_ref())
            .unwrap_or_else(|e| fail(&e)),
    }
}

/// The current subject's plan as a skeleton (condition.rs: its values replaced by numbered
/// placeholders) and its values: what a compiled filter is built from at planning, and checks
/// and reads at execution. None without a subject.
fn plan_skeleton(object_type: &str, permission: &str) -> Option<(String, Vec<serde_json::Value>)> {
    skeleton_or_error(object_type, permission).unwrap_or_else(|e| fail(&e))
}

fn skeleton_or_error(object_type: &str, permission: &str) -> Result<Option<(String, Vec<serde_json::Value>)>, String> {
    let Some(subject) = subject::current() else { return Ok(None) };
    let plan = sidecar::plan(&subject, object_type, permission, subject::principal_key(), subject::principal)?;
    let (skeleton, values) = match &*plan {
        Plan::Always => (serde_json::json!({"plan": "always"}), Vec::new()),
        Plan::Never => (serde_json::json!({"plan": "never"}), Vec::new()),
        Plan::Rows => (serde_json::json!({"plan": "rows"}), Vec::new()),
        Plan::Condition(c) => {
            let (skeleton, values) = crate::condition::parameterized(c);
            (serde_json::json!({"plan": "condition", "condition": skeleton}), values)
        }
    };
    Ok(Some((skeleton.to_string(), values)))
}

/// For the planner hook: the current subject's plan skeleton, to compile into a filter;
/// `{"plan":"none"}` without a subject (every compiled filter answers it: its values are null).
/// NULL when the plan cannot be had (the sidecar unreachable): the planner then keeps the per-row
/// policy, which fails closed when the query runs, and a check made at the end of a statement
/// counts it as changed rather than failing that statement.
#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn resource_plan(object_type: &str, permission: &str) -> Option<String> {
    resource_plan_now(object_type, permission)
}

/// `authz.resource_plan`, for code that checked the subject is resolved (subject::resolved).
pub fn resource_plan_now(object_type: &str, permission: &str) -> Option<String> {
    match skeleton_or_error(object_type, permission) {
        Ok(Some((skeleton, _))) => Some(skeleton),
        Ok(None) => Some(NO_SUBJECT.into()),
        Err(_) => None,
    }
}

const NO_SUBJECT: &str = r#"{"plan":"none"}"#;

/// A compiled filter's guard: true when the current subject's plan has the skeleton the filter
/// was compiled from. A cached plan reused by a subject whose plan has another shape is rebuilt
/// before its next statement (rewrite/statements.rs, like a subject of another breadth); should one run
/// anyway, it fails rather than filter with another subject's shape.
#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn plan_shape_is(object_type: &str, permission: &str, shape: &str) -> bool {
    match plan_skeleton(object_type, permission) {
        Some((skeleton, _)) if skeleton == shape => true,
        // no subject: the filter's values are all null, nothing is allowed
        None => true,
        Some(_) => {
            ereport!(
                ERROR,
                PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
                format!("pg_auth: this statement's plan was built for another shape of {object_type}#{permission}'s policy; run it again")
            );
        }
    }
}

/// The current subject's `n`th plan value as text, when it is valid input for `typ` (None
/// otherwise: the comparison is unknown and allows nothing, rather than an error).
#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn plan_value(object_type: &str, permission: &str, n: i32, typ: pg_sys::Oid) -> Option<String> {
    let (_, values) = plan_skeleton(object_type, permission)?;
    valid_input(crate::condition::as_text(values.get(usize::try_from(n).ok()?)?)?, typ)
}

/// The same for a list value: its items valid for `typ`.
#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn plan_values(object_type: &str, permission: &str, n: i32, typ: pg_sys::Oid) -> Option<Vec<String>> {
    let (_, values) = plan_skeleton(object_type, permission)?;
    let items = values.get(usize::try_from(n).ok()?)?.as_array()?;
    Some(items.iter().filter_map(crate::condition::as_text).filter_map(|v| valid_input(v, typ)).collect())
}

fn valid_input(value: String, typ: pg_sys::Oid) -> Option<String> {
    crate::ffi::valid_input(&value, typ).then_some(value)
}

/// Prepare this backend for its first request, so none of it is paid while a query is planned:
/// load the configuration and the hierarchy declarations, prepare pg_auth's own queries, connect
/// to the sidecar, and, when the session already has a subject, fetch its lists and plans for every
/// protected table. Call it once per connection (a pooler's connect query); calling it again is
/// cheap. One row per step.
#[pg_extern(volatile, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn warm() -> TableIterator<'static, (name!(step, String), name!(detail, String))> {
    let mut steps = vec![
        ("configuration".to_string(), format!("{} settings", subject::warm())),
        ("hierarchies".to_string(), format!("{} declared", sidecar::warm_hierarchies())),
    ];
    let queries: Vec<_> = subject::queries().into_iter().chain(sidecar::queries()).collect();
    let prepared = queries.iter().filter(|(q, types)| crate::ffi::prepare(q, types)).count();
    steps.push(("queries".into(), format!("{prepared} of {} prepared", queries.len())));
    steps.push((
        "sidecar".into(),
        match sidecar::ping() {
            Ok(engine) => format!("connected: {engine}"),
            Err(e) => e,
        },
    ));
    if let Some(subject) = subject::current() {
        // every protected (type, permission), and whether it is decided on the rows' attributes
        let bindings = crate::ffi::read_one::<Vec<String>>(
            "SELECT array_agg(DISTINCT object_type || ' ' || permission || ' ' || (attributes IS NOT NULL)) FROM authz.bindings",
            &[],
        )
        .unwrap_or_default();
        let mut fetched = 0;
        for binding in &bindings {
            let mut parts = binding.split(' ');
            let (Some(object_type), Some(permission), Some(attrs)) = (parts.next(), parts.next(), parts.next()) else { continue };
            let ok = if attrs == "true" {
                sidecar::plan(&subject, object_type, permission, subject::principal_key(), subject::principal).is_ok()
            } else {
                sidecar::list(&subject, object_type, permission).is_ok()
            };
            fetched += usize::from(ok);
        }
        steps.push(("subject".into(), format!("{subject}: {fetched} of {} lists and plans fetched", bindings.len())));
    }
    TableIterator::new(steps)
}

/// The ids in `rel.col` the current subject may access, for an indexable `col = ANY(...)` filter.
/// The planner uses this form when the subject's set was small enough to list. A cached plan can
/// be reused by a subject whose set is not (prepared statements across requests): then the
/// column's distinct values are batch-checked instead, which keeps the plan correct.
/// Each id converted by `convert` (None drops it), read from the listed set without copying it:
/// the array a scan's `= ANY` is built from, every statement.
fn allowed<T>(object_type: &str, permission: &str, rel: pg_sys::Oid, col: &str, convert: impl Fn(&str) -> Option<T>) -> Vec<T> {
    let Some(subject) = subject::current() else { return Vec::new() };
    match sidecar::list(&subject, object_type, permission).as_deref() {
        Ok(Listed::Ids(ids)) => ids.iter().filter_map(|id| convert(id)).collect(),
        Ok(Listed::TooMany) => {
            // the subject's fresh permission index set, when it has one: no reading the column
            if let Some(set) = sidecar::index_set_loaded(&subject, object_type, permission, usize::MAX) {
                return set.ids.iter().filter_map(|id| convert(id)).collect();
            }
            // else the column's distinct values, asking the engine
            let candidates = distinct_values(rel, col, object_type);
            let ids: Vec<Option<String>> = candidates.iter().cloned().map(Some).collect();
            let allowed = sidecar::permits_many(&subject, object_type, permission, &ids).unwrap_or_else(|e| fail(&e));
            candidates.iter().zip(allowed).filter(|(_, ok)| *ok).filter_map(|(id, _)| convert(id)).collect()
        }
        Err(e) => fail(e),
    }
}

/// Every distinct non-null value of `rel.col`, read as the extension owner.
fn distinct_values(rel: pg_sys::Oid, col: &str, object_type: &str) -> Vec<String> {
    Spi::connect(|client| {
        let table = client
            .select("select $1::regclass::text", None, &[rel.into()])
            .and_then(|mut r| r.next().map(|row| row.get::<String>(1)).transpose())
            .ok()
            .flatten()
            .flatten()
            .unwrap_or_else(|| fail(&format!("relation {rel:?} does not exist")));
        // a key expression comes from the table's binding (the bound table or a partition's
        // ancestor; pg_partition_ancestors lists no row for a table that is not a partition),
        // never from the query
        let key = if col.is_empty() {
            client
                .select(
                    "select key_expr from authz.bindings where (rel = $1 or rel in (select relid from pg_partition_ancestors($1))) and key_expr is not null and object_type = $2 limit 1",
                    None,
                    &[rel.into(), object_type.into()],
                )
                .ok()
                .and_then(|mut r| r.next().and_then(|row| row.get::<String>(1).ok().flatten()))
                .map_or_else(|| fail(&format!("relation {rel:?} has no key expression")), |expr| format!("({expr})"))
        } else {
            // only a protected table's id column (or a partition's), or a declared hierarchy's:
            // these functions run as the extension owner, and a caller names the table
            let bound = client
                .select(
                    "select exists (select from authz.bindings where (rel = $1 or rel in (select relid from pg_partition_ancestors($1))) and $2 in (column_name, insert_column))
                         or exists (select from authz.hierarchies where source = $1 and id_column = $2)",
                    None,
                    &[rel.into(), col.into()],
                )
                .ok()
                .and_then(|mut r| r.next().and_then(|row| row.get::<bool>(1).ok().flatten()))
                .unwrap_or(false);
            if !bound {
                fail(&format!("{table}.{col} is not a protected column"));
            }
            quote_ident(col)
        };
        let sql = format!("select distinct {key}::text from {table} where {key} is not null");
        client
            .select(&sql, None, &[])
            .unwrap_or_else(|e| fail(&e.to_string()))
            .filter_map(|row| row.get::<String>(1).ok().flatten())
            .collect()
    })
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_int4(object_type: &str, permission: &str, rel: pg_sys::Oid, col: &str) -> Vec<i32> {
    allowed(object_type, permission, rel, col, canonical_int)
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_int8(object_type: &str, permission: &str, rel: pg_sys::Oid, col: &str) -> Vec<i64> {
    allowed(object_type, permission, rel, col, canonical_int)
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_uuid(object_type: &str, permission: &str, rel: pg_sys::Oid, col: &str) -> Vec<pgrx::Uuid> {
    allowed(object_type, permission, rel, col, canonical_uuid)
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_text(object_type: &str, permission: &str, rel: pg_sys::Oid, col: &str) -> Vec<String> {
    allowed(object_type, permission, rel, col, |s| Some(s.to_string()))
}

/// The one id the current subject may access (None: none), for a `col = (SELECT ...)` filter: the
/// planner uses this form for a subject with at most one object, since `col = ANY(...)` loses the
/// order an index on (col, ...) gives. AuthzBatch reads its other child for a subject with more,
/// so this only answers for one with more when a plan without AuthzBatch is reused: it fails
/// then, rather than show a subset.
fn allowed_one(object_type: &str, permission: &str) -> Option<String> {
    let subject = subject::current()?;
    match sidecar::list(&subject, object_type, permission).as_deref() {
        Ok(Listed::Ids(ids)) if ids.len() <= 1 => ids.iter().next().cloned(),
        Ok(_) => single_plan_reused(),
        Err(e) => fail(e),
    }
}

fn single_plan_reused() -> ! {
    ereport!(
        ERROR,
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        "pg_auth: this plan was made for a subject with at most one object and cannot answer for this one; plan the statement again (DISCARD PLANS)"
    );
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_one_int4(object_type: &str, permission: &str, _rel: pg_sys::Oid, _col: &str) -> Option<i32> {
    allowed_one(object_type, permission).and_then(|s| canonical_int(&s))
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_one_int8(object_type: &str, permission: &str, _rel: pg_sys::Oid, _col: &str) -> Option<i64> {
    allowed_one(object_type, permission).and_then(|s| canonical_int(&s))
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_one_uuid(object_type: &str, permission: &str, _rel: pg_sys::Oid, _col: &str) -> Option<pgrx::Uuid> {
    allowed_one(object_type, permission).and_then(|s| canonical_uuid(&s))
}

#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn allowed_one_text(object_type: &str, permission: &str, _rel: pg_sys::Oid, _col: &str) -> Option<String> {
    allowed_one(object_type, permission)
}

/// `s` as an integer, when it is written the way Postgres writes that integer (`col::text`, which
/// the per-row check compares): "007", "+7" and "-0" are other objects than 7 and 0.
pub fn canonical_int<T: std::str::FromStr>(s: &str) -> Option<T> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let written = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && (!digits.starts_with('0') || s == "0");
    if written { s.parse().ok() } else { None }
}

/// A uuid written as Postgres writes it (lower case, hyphenated 8-4-4-4-12), as bytes.
fn canonical_uuid(s: &str) -> Option<pgrx::Uuid> {
    let written = s.len() == 36
        && s.bytes()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == b'-' } else { c.is_ascii_digit() || (b'a'..=b'f').contains(&c) });
    if written { uuid_bytes(s).map(pgrx::Uuid::from_bytes) } else { None }
}

fn uuid_bytes(s: &str) -> Option<[u8; 16]> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Does `subject` ('user:anne') have `permission` on `object` ('document:1')? Asks the engine, for
/// debugging protected tables. Restricted to administrators: it answers for any subject.
#[pg_extern(volatile, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn check(subject: &str, permission: &str, object: &str) -> bool {
    let (object_type, id) = split_ref(object);
    split_ref(subject);
    // the engine itself: the index and declared hierarchies answer for the current subject
    !sidecar::filter(subject, object_type, permission, vec![id.to_string()]).unwrap_or_else(|e| fail(&e)).is_empty()
}

/// The current subject's own check; safe for everyone.
#[pg_extern(stable, parallel_restricted, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn can(permission: &str, object: &str) -> bool {
    let (object_type, id) = split_ref(object);
    permits(object_type, permission, Some(id))
}

/// This backend's round trips to the sidecar since it started: how many, how many ids were sent
/// to be checked, the time spent waiting on the sidecar, how much of it the sidecar spent
/// waiting on the engine, and how many ids were answered from the permission index instead.
/// Cumulative; diff two readings to cost one statement.
#[pg_extern(volatile)]
fn stats() -> TableIterator<
    'static,
    (name!(sidecar_calls, i64), name!(ids_checked, i64), name!(sidecar_ms, f64), name!(engine_ms, f64), name!(index_lookups, i64)),
> {
    let s = sidecar::stats();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    TableIterator::once((to_i64(s.calls), to_i64(s.ids_checked), ms(s.sidecar), ms(s.engine), to_i64(s.index_lookups)))
}

/// Which engine the sidecar talks to, or why it cannot be reached.
#[pg_extern(volatile, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn engine() -> String {
    sidecar::ping().unwrap_or_else(|e| format!("unreachable: {e}"))
}

fn split_ref(r: &str) -> (&str, &str) {
    match r.split_once(':') {
        Some((t, id)) if !t.is_empty() && !id.is_empty() => (t, id),
        _ => error!("expected type:id, got {r:?}"),
    }
}
