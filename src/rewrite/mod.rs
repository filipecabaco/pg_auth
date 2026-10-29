//! Planner integration: shape each protected table's scan with the engine's answer.
//!
//! `authz.bind` installs the policy `authz.permits('<type>', '<permission>', col::text)`, which
//! asks the engine (through the sidecar) per row. At planning time this hook asks the sidecar
//! once for the current subject and, when the subject's allowed set is small enough to list,
//! replaces that policy predicate with
//!     col = ANY ((SELECT authz.allowed_<type>('<type>', '<permission>', <table>, '<col>')))
//! an indexable filter evaluated once per execution: an empty set reads no rows, a few ids use
//! the index. Larger sets keep the per-row predicate.
//!
//! The predicate stays in the relation's securityQuals, so row level security's ordering still
//! keeps non-leakproof user functions away from rows the subject may not see. The ids are fetched
//! at execution for whoever runs the plan, so a cached plan reused by another subject never
//! applies someone else's answer.
//!
//! Where the per-row predicate stays (too many objects to list), reads are planned as an
//! `AuthzBatch` custom scan instead: it pulls rows from the table scan in growing batches, checks
//! their ids with one sidecar request per batch, and only then evaluates the other conditions.
//! The inner scan keeps only leakproof conditions on the table itself, so the ordering guarantee
//! above holds. Writes and row locking keep the per-row predicate.

use crate::pg::{Ptr, pg_sys};
use pgrx::prelude::*;
use pgrx::{GucContext, GucFlags, GucRegistry, GucSetting};

pub mod batch;
mod compile;
mod guard;
mod paths;
mod statements;

pub static PLAN_LOOKUP: GucSetting<bool> = GucSetting::<bool>::new(true);
pub static BATCH_CHECK: GucSetting<bool> = GucSetting::<bool>::new(true);
pub static REPLAN_ON_BREADTH: GucSetting<bool> = GucSetting::<bool>::new(true);

/// How a protected table is enforced, and why; collected by `authz.explain`.
pub struct Decision {
    pub table: String,
    pub mode: &'static str,
    pub reason: String,
    /// what would make it cheaper, when there is something to say
    pub advice: Option<String>,
}

thread_local! {
    static TRACE: std::cell::RefCell<Option<Vec<Decision>>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` and return the decisions the planner hook made meanwhile. Tracing ends however `f`
/// does (an error in the traced query unwinds through here).
pub fn trace(f: impl FnOnce()) -> Vec<Decision> {
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            TRACE.with(|t| t.borrow_mut().take());
        }
    }
    TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
    let _stop = Stop;
    f();
    TRACE.with(|t| t.borrow_mut().take()).unwrap_or_default()
}

fn tracing() -> bool {
    TRACE.with(|t| t.borrow().is_some())
}

const LISTED: &str = "listed ids";
const PER_ROW: &str = "per-row check";
const BATCHED: &str = "batched check";

fn record(rte: Ptr<'_, pg_sys::RangeTblEntry>, mode: &'static str, reason: String) {
    record_advised(rte, mode, reason, None);
}

fn record_advised(rte: Ptr<'_, pg_sys::RangeTblEntry>, mode: &'static str, reason: String, advice: Option<String>) {
    if !tracing() {
        return;
    }
    let table = crate::pg::catalog::relation_name(rte.relid());
    TRACE.with(|t| {
        if let Some(v) = t.borrow_mut().as_mut() {
            v.push(Decision { table, mode, reason, advice });
        }
    });
}

pub fn init() {
    GucRegistry::define_bool_guc(
        c"pg_auth.plan_lookup",
        c"Ask the authorization sidecar at planning time to shape protected scans.",
        c"When off, every protected row is checked through authz.permits.",
        &PLAN_LOOKUP,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"pg_auth.batch_check",
        c"Check the rows of reads that cannot use listed ids in batches (AuthzBatch scan).",
        c"When off, such rows are checked one by one through authz.permits.",
        &BATCH_CHECK,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"pg_auth.replan_on_breadth",
        c"Rebuild this backend's cached plans when a request's subject is much broader or narrower than the one they were built for.",
        c"A cached plan is shaped by how many objects its planning subject may access; when off, it is reused by every subject.",
        &REPLAN_ON_BREADTH,
        GucContext::Userset,
        GucFlags::default(),
    );
    crate::pg::hooks::install(crate::pg::hooks::Hooks {
        planner: |query| {
            if PLAN_LOOKUP.get() || tracing() {
                guard::shape(query);
            }
        },
        executor_start: statements::executor_start,
        executor_ending: statements::executor_ending,
        executor_ended: statements::executor_ended,
        utility: statements::utility,
        xact: statements::xact,
        subxact: statements::subxact,
    });
    crate::pg::plan::install(crate::pg::plan::PlannerHooks {
        rel_pathlist: |root, rel, rti, rte| {
            if BATCH_CHECK.get() {
                paths::add_batch_paths(root, rel, rti, rte);
            }
        },
        plan_custom: paths::plan_batch,
        scan_methods: &crate::pg::exec::SCAN_METHODS,
    });
}

/// Whether `expr`, read as an expression over `rel`'s columns, is IMMUTABLE: authz.protect's key
/// expressions must be, since their value for a row may not change. Only parses (Postgres raises
/// its own error when the expression does not fit the relation); nothing is executed.
#[pg_extern(volatile)]
fn key_is_immutable(rel: pg_sys::Oid, expr: &str) -> bool {
    crate::pg::catalog::expression_is_immutable(rel, expr)
}
