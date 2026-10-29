//! Statements: frame each top-level statement for advice (advice.rs), and rebuild this backend's
//! cached plans when the subject changes breadth or shape (sidecar.rs).

use super::{PLAN_LOOKUP, REPLAN_ON_BREADTH};
use crate::pg::hooks::{self, StatementSnapshot};
use crate::pg::{Node, pg_sys};
use crate::sidecar;
use std::cell::{Cell, RefCell};

thread_local! {
    /// executor nesting: pg_auth's own queries and functions' statements count toward the
    /// top-level statement
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// DEPTH as each open subtransaction began
    static SUB_DEPTHS: RefCell<Vec<(pg_sys::SubTransactionId, u32)>> = const { RefCell::new(Vec::new()) };
}

/// A statement that errors never reaches ExecutorEnd: an aborted transaction leaves no statement
/// running.
pub fn xact(event: pg_sys::XactEvent::Type) {
    use pg_sys::XactEvent::{
        XACT_EVENT_ABORT, XACT_EVENT_COMMIT, XACT_EVENT_PARALLEL_ABORT, XACT_EVENT_PARALLEL_COMMIT, XACT_EVENT_PREPARE,
    };
    if matches!(event, XACT_EVENT_ABORT | XACT_EVENT_PARALLEL_ABORT) {
        DEPTH.set(0);
    }
    if matches!(event, XACT_EVENT_ABORT | XACT_EVENT_PARALLEL_ABORT | XACT_EVENT_COMMIT | XACT_EVENT_PARALLEL_COMMIT | XACT_EVENT_PREPARE) {
        SUB_DEPTHS.with(|s| s.borrow_mut().clear());
    }
}

/// An error a savepoint or an EXCEPTION block catches ends its statements without ExecutorEnd:
/// back to the depth the subtransaction began at.
pub fn subxact(event: pg_sys::SubXactEvent::Type, sub: pg_sys::SubTransactionId) {
    use pg_sys::SubXactEvent::{SUBXACT_EVENT_ABORT_SUB, SUBXACT_EVENT_COMMIT_SUB, SUBXACT_EVENT_START_SUB};
    SUB_DEPTHS.with(|s| {
        let mut s = s.borrow_mut();
        match event {
            SUBXACT_EVENT_START_SUB => s.push((sub, DEPTH.get())),
            SUBXACT_EVENT_COMMIT_SUB | SUBXACT_EVENT_ABORT_SUB => {
                if let Some(i) = s.iter().rposition(|(id, _)| *id == sub) {
                    if event == SUBXACT_EVENT_ABORT_SUB {
                        DEPTH.set(s[i].1);
                    }
                    s.truncate(i);
                }
            }
            _ => {}
        }
    });
}

pub fn executor_start() {
    if DEPTH.get() == 0 {
        sidecar::statement_reset();
    }
    DEPTH.set(DEPTH.get() + 1);
}

pub fn executor_ending(snapshot: &StatementSnapshot) {
    if DEPTH.get() == 1 {
        // a top-level portal ends after popping its snapshot: check with the statement's own
        snapshot.active(replan_if_breadth_changed);
    }
}

pub fn executor_ended() {
    DEPTH.set(DEPTH.get().saturating_sub(1));
    if DEPTH.get() == 0 {
        crate::advice::statement_end();
    }
}

/// After a utility statement: SET pg_auth.subject_id / request.jwt.claims (at any level: a
/// function may set them), only with a snapshot already active: taking one here would fix a
/// REPEATABLE READ transaction's snapshot early; a top-level SET is followed up after the next
/// statement.
pub fn utility(stmt: Node<'_>) {
    let Some(set) = stmt.cast::<pg_sys::VariableSetStmt>() else { return };
    if !hooks::in_transaction() {
        return;
    }
    if hooks::snapshot_active() {
        replan_if_breadth_changed();
    } else if sidecar::shapes_planned() && set.name().is_some_and(|name| crate::subject::reads(&name)) {
        // a top-level SET cannot be compared yet (the next statement compares it, too late for a
        // compiled filter, whose plan must fit the principal's shape): one of a setting the
        // subject comes from discards the plans holding compiled filters now
        hooks::reset_plan_cache();
        sidecar::forget_planned();
    }
}

/// After a top-level statement (PostgREST sets the request's claims with one) or a SET: when
/// the subject now falls in another breadth class than this backend's cached plans were built
/// for, mark them all invalid, so the next execution of each plans again, for this subject.
fn replan_if_breadth_changed() {
    if !PLAN_LOOKUP.get() || !REPLAN_ON_BREADTH.get() || !sidecar::breadth_planned() {
        return;
    }
    let subject = crate::subject::current_as_caller();
    // both compared, so each starts over with the plans
    if sidecar::breadth_changed(subject.as_deref()) | sidecar::shapes_changed() {
        hooks::reset_plan_cache();
        sidecar::forget_planned();
    }
}
