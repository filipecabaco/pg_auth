//! Whether pg_auth is applying its own policies in this transaction (authz_internal.bind, the
//! partition trigger): the trigger does nothing then, since applying runs ALTER TABLE, which fires
//! it again. Held in the backend, not in a setting a role could change to turn the trigger off,
//! and reset when the transaction ends.

use pgrx::prelude::*;
use pgrx::{PgXactCallbackEvent, register_xact_callback};
use std::cell::Cell;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

#[pg_extern(volatile, schema = "authz_internal")]
fn applying_begin() {
    if !ARMED.with(|a| a.replace(true)) {
        let reset = || {
            DEPTH.with(|d| d.set(0));
            ARMED.with(|a| a.set(false));
        };
        register_xact_callback(PgXactCallbackEvent::Commit, reset);
        register_xact_callback(PgXactCallbackEvent::Abort, reset);
    }
    DEPTH.with(|d| d.set(d.get() + 1));
}

#[pg_extern(volatile, schema = "authz_internal")]
fn applying_end() {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

#[pg_extern(volatile, schema = "authz_internal")]
fn applying() -> bool {
    DEPTH.with(|d| d.get() > 0)
}
