//! Advice for statements that were heavy on the authorization engine: what they cost, why, and
//! what to change. Every top-level statement that asked pg_auth anything is summarised per
//! (subject, type, permission); `authz.last_statement_advice()` returns the last one (it works
//! behind PostgREST, where warnings never reach the client), and `pg_auth.warn_ms` raises a
//! WARNING with the same advice when a statement's engine time reaches it.

use crate::sidecar::{self, Activity};
use pgrx::pg_sys::panic::ErrorReport;
use pgrx::prelude::*;
use pgrx::{GucContext, GucFlags, GucRegistry, GucSetting};
use std::cell::RefCell;

pub static WARN_MS: GucSetting<i32> = GucSetting::<i32>::new(-1);

pub fn init() {
    GucRegistry::define_int_guc(
        c"pg_auth.warn_ms",
        c"Warn when a statement spends at least this long checking rows with the authorization engine.",
        c"-1 (default) never warns; the advice is also returned by authz.last_statement_advice().",
        &WARN_MS,
        -1,
        3_600_000,
        GucContext::Userset,
        GucFlags::UNIT_MS,
    );
}

#[derive(Clone)]
struct Row {
    subject: String,
    object_type: String,
    permission: String,
    ids_checked: i64,
    engine_ms: f64,
    index_lookups: i64,
    advice: Option<String>,
}

thread_local! {
    static LAST: RefCell<Vec<Row>> = const { RefCell::new(Vec::new()) };
}

/// Why rows of this subject went to the engine, and what would answer them from Postgres.
fn advise(subject: &str, object_type: &str, a: &Activity) -> Option<String> {
    if a.ids_checked == 0 {
        return None;
    }
    let waited = |d: &std::time::Duration| format!("{:.0} ms", ms(*d));
    let why = if a.index_off {
        "pg_auth.permission_index is off: turn it on to answer these rows from Postgres".to_string()
    } else if let Some((d, true)) = a.fill_wait.as_ref().filter(|_| a.index_lookups > 0) {
        format!(
            "then it waited {} for its permission index set to fill and answered the rest from Postgres, as later reads will",
            waited(d)
        )
    } else if let Some((d, true)) = &a.fill_wait {
        format!(
            "then it waited {} for its permission index set to fill, which this transaction cannot see (REPEATABLE READ or SERIALIZABLE keep the snapshot it started with): later transactions answer from Postgres",
            waited(d)
        )
    } else if let Some((d, false)) = &a.fill_wait {
        format!(
            "it waited {} (pg_auth.fill_wait_ms) for its permission index set, which did not become fresh: later reads answer from Postgres once it is",
            waited(d)
        )
    } else {
        match a.fill_queued {
            Some(false) => "the sidecar keeps no permission index: set PG_AUTH_DATABASE_URL on it to answer these rows from Postgres",
            Some(true) => "its permission index set did not exist yet and is being filled: later reads answer from Postgres",
            None => "its permission index set is not fresh (being filled, or refilled after the engine's data changed): later reads answer from Postgres once it is",
        }
        .to_string()
    };
    Some(format!(
        "{subject} may access more {object_type} objects than the engine lists, so {} ids were checked with the engine ({:.0} ms); {why}.",
        a.ids_checked,
        ms(a.check_engine)
    ))
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Summarise the statement that just ended (called from the executor hook in rewrite/statements.rs).
pub fn statement_end() {
    let activity = sidecar::statement_activity();
    if activity.is_empty() {
        return;
    }
    let mut rows: Vec<Row> = activity
        .into_iter()
        .map(|((subject, object_type, permission), a)| Row {
            advice: advise(&subject, &object_type, &a),
            ids_checked: crate::access::to_i64(a.ids_checked),
            engine_ms: ms(a.check_engine + a.list_engine),
            index_lookups: crate::access::to_i64(a.index_lookups),
            subject,
            object_type,
            permission,
        })
        .collect();
    rows.sort_by(|a, b| b.engine_ms.total_cmp(&a.engine_ms));

    let ids: i64 = rows.iter().map(|r| r.ids_checked).sum();
    let engine: f64 = rows.iter().map(|r| r.engine_ms).sum();
    let warn = WARN_MS.get();
    if warn >= 0 && ids > 0 && engine >= f64::from(warn) {
        let hint = rows.iter().filter_map(|r| r.advice.clone()).collect::<Vec<_>>().join("\n");
        ErrorReport::new(
            PgSqlErrorCode::ERRCODE_WARNING,
            format!("pg_auth: this statement spent {engine:.0} ms in the authorization engine checking {ids} ids"),
            "pg_auth",
        )
        .set_hint(hint)
        .report(PgLogLevel::WARNING);
    }
    LAST.with(|l| *l.borrow_mut() = rows);
}

/// The last statement that asked pg_auth anything, per subject and permission: ids checked with
/// the engine, engine time, ids answered from the permission index, and advice when the engine
/// had to check rows.
#[pg_extern(volatile)]
#[expect(clippy::type_complexity, reason = "pgrx reads the column names and types from this literal return type")]
fn last_statement_advice() -> TableIterator<
    'static,
    (
        name!(subject, String),
        name!(object_type, String),
        name!(permission, String),
        name!(ids_checked, i64),
        name!(engine_ms, f64),
        name!(index_lookups, i64),
        name!(advice, Option<String>),
    ),
> {
    let rows = LAST.with(|l| l.borrow().clone());
    TableIterator::new(
        rows.into_iter().map(|r| (r.subject, r.object_type, r.permission, r.ids_checked, r.engine_ms, r.index_lookups, r.advice)),
    )
}
