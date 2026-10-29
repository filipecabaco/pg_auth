//! Postgres's hooks and callbacks: each entry point turns what Postgres passes into handles and
//! calls pg_auth's safe code (`Hooks`), then the hook it replaced.

use super::{Node, Ptr, pg_sys};
use pgrx::pg_guard;
use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

/// pg_auth's side of each hook.
pub struct Hooks {
    /// before the query is planned
    pub planner: for<'a> fn(Ptr<'a, pg_sys::Query>),
    /// as a statement's executor starts
    pub executor_start: fn(),
    /// as it ends, before the executor shuts it down: with the statement's snapshot at hand
    pub executor_ending: fn(&StatementSnapshot),
    /// after it shut down
    pub executor_ended: fn(),
    /// after a utility statement ran
    pub utility: for<'a> fn(Node<'a>),
    pub xact: fn(pg_sys::XactEvent::Type),
    pub subxact: fn(pg_sys::SubXactEvent::Type, pg_sys::SubTransactionId),
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();
static PREV_PLANNER: OnceLock<pg_sys::planner_hook_type> = OnceLock::new();
static PREV_EXECUTOR_START: OnceLock<pg_sys::ExecutorStart_hook_type> = OnceLock::new();
static PREV_EXECUTOR_END: OnceLock<pg_sys::ExecutorEnd_hook_type> = OnceLock::new();
static PREV_PROCESS_UTILITY: OnceLock<pg_sys::ProcessUtility_hook_type> = OnceLock::new();

/// Install the hooks (once, from _PG_init).
pub fn install(hooks: Hooks) {
    if HOOKS.set(hooks).is_err() {
        return;
    }
    // SAFETY: _PG_init runs once per backend, before any query, on its only thread: the hook
    // variables are read and replaced with nothing else running
    unsafe {
        let _ = PREV_PLANNER.set(pg_sys::planner_hook);
        pg_sys::planner_hook = Some(planner);
        let _ = PREV_EXECUTOR_START.set(pg_sys::ExecutorStart_hook);
        pg_sys::ExecutorStart_hook = Some(executor_start);
        let _ = PREV_EXECUTOR_END.set(pg_sys::ExecutorEnd_hook);
        pg_sys::ExecutorEnd_hook = Some(executor_end);
        let _ = PREV_PROCESS_UTILITY.set(pg_sys::ProcessUtility_hook);
        pg_sys::ProcessUtility_hook = Some(process_utility);
        pg_sys::RegisterXactCallback(Some(xact_callback), std::ptr::null_mut());
        pg_sys::RegisterSubXactCallback(Some(subxact_callback), std::ptr::null_mut());
    }
}

fn hooks() -> &'static Hooks {
    HOOKS.get().unwrap_or_else(|| unreachable!("the entry points are installed with their hooks"))
}

#[pg_guard]
unsafe extern "C-unwind" fn planner(
    parse: *mut pg_sys::Query,
    query_string: *const c_char,
    cursor_options: c_int,
    bound_params: pg_sys::ParamListInfo,
) -> *mut pg_sys::PlannedStmt {
    // SAFETY: the planner hook is given the query being planned, live for the call
    if let Some(query) = unsafe { Ptr::from_raw(parse) } {
        (hooks().planner)(query);
    }
    // SAFETY: the planner it replaced, or the standard one, with the arguments as given
    unsafe {
        match PREV_PLANNER.get().copied().flatten() {
            Some(prev) => prev(parse, query_string, cursor_options, bound_params),
            None => pg_sys::standard_planner(parse, query_string, cursor_options, bound_params),
        }
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn executor_start(query_desc: *mut pg_sys::QueryDesc, eflags: c_int) {
    (hooks().executor_start)();
    // SAFETY: the hook it replaced, or the standard one, with the arguments as given
    unsafe {
        match PREV_EXECUTOR_START.get().copied().flatten() {
            Some(prev) => prev(query_desc, eflags),
            None => pg_sys::standard_ExecutorStart(query_desc, eflags),
        }
    }
}

/// The snapshot of a statement whose executor is ending, to run code under (a top-level portal
/// ends after popping it).
pub struct StatementSnapshot(pg_sys::Snapshot);

impl StatementSnapshot {
    /// Run `f` with the statement's snapshot active when no snapshot is.
    pub fn active<R>(&self, f: impl FnOnce() -> R) -> R {
        // SAFETY: the statement's snapshot is live until its executor ends, which waits for this
        let push = !snapshot_active() && !self.0.is_null();
        if push {
            // SAFETY: as above; popped again below, also when `f` raises (the guard's unwinding
            // reaches the transaction abort, which resets the active snapshot stack)
            unsafe { pg_sys::PushActiveSnapshot(self.0) };
        }
        let result = f();
        if push {
            // SAFETY: the snapshot pushed above is the active one
            unsafe { pg_sys::PopActiveSnapshot() };
        }
        result
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn executor_end(query_desc: *mut pg_sys::QueryDesc) {
    // SAFETY: the hook is given the live descriptor of the statement ending
    let snapshot = StatementSnapshot(unsafe { (*query_desc).snapshot });
    (hooks().executor_ending)(&snapshot);
    // SAFETY: the hook it replaced, or the standard one, with the argument as given
    unsafe {
        match PREV_EXECUTOR_END.get().copied().flatten() {
            Some(prev) => prev(query_desc),
            None => pg_sys::standard_ExecutorEnd(query_desc),
        }
    }
    (hooks().executor_ended)();
}

#[pg_guard]
#[allow(clippy::too_many_arguments)]
unsafe extern "C-unwind" fn process_utility(
    pstmt: *mut pg_sys::PlannedStmt,
    query_string: *const c_char,
    read_only_tree: bool,
    context: pg_sys::ProcessUtilityContext::Type,
    params: pg_sys::ParamListInfo,
    query_env: *mut pg_sys::QueryEnvironment,
    dest: *mut pg_sys::DestReceiver,
    qc: *mut pg_sys::QueryCompletion,
) {
    // SAFETY: the hook it replaced, or the standard one, with the arguments as given
    unsafe {
        match PREV_PROCESS_UTILITY.get().copied().flatten() {
            Some(prev) => prev(pstmt, query_string, read_only_tree, context, params, query_env, dest, qc),
            None => pg_sys::standard_ProcessUtility(pstmt, query_string, read_only_tree, context, params, query_env, dest, qc),
        }
    }
    // SAFETY: the statement Postgres just ran, live for this call
    if let Some(stmt) = unsafe { Ptr::from_raw(pstmt) }.and_then(|p| {
        // SAFETY: a live PlannedStmt's utility statement is a node of it, or null
        unsafe { Ptr::from_raw((*p.as_raw()).utilityStmt) }
    }) {
        (hooks().utility)(stmt);
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn xact_callback(event: pg_sys::XactEvent::Type, _arg: *mut c_void) {
    (hooks().xact)(event);
}

#[pg_guard]
unsafe extern "C-unwind" fn subxact_callback(
    event: pg_sys::SubXactEvent::Type,
    sub: pg_sys::SubTransactionId,
    _parent: pg_sys::SubTransactionId,
    _arg: *mut c_void,
) {
    (hooks().subxact)(event, sub);
}

/// Whether a snapshot is active (a statement is running with one).
pub fn snapshot_active() -> bool {
    // SAFETY: reads the active snapshot stack
    unsafe { pg_sys::ActiveSnapshotSet() }
}

/// Whether a transaction is in progress (not aborted).
pub fn in_transaction() -> bool {
    // SAFETY: reads the transaction state
    unsafe { pg_sys::IsTransactionState() }
}

/// Mark every cached plan of this backend invalid: each plans again on its next execution.
pub fn reset_plan_cache() {
    // SAFETY: invalidates the plan cache, as a relcache flush does
    unsafe { pg_sys::ResetPlanCache() }
}
