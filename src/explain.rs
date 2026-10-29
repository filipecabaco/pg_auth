//! `authz.explain(query)`: how pg_auth enforces each protected table in a statement, and why.

use pgrx::prelude::*;

/// Plans `query` without running it, as the caller, and reports the mode chosen for each table
/// protected by pg_auth, the reason, and advice when the table will be checked with the engine.
#[pg_extern(volatile)]
fn explain(
    query: &str,
) -> TableIterator<'static, (name!(table_name, String), name!(mode, String), name!(reason, String), name!(advice, Option<String>))> {
    let decisions = crate::rewrite::trace(|| {
        // without assigning a transaction id, so it also works on a read replica
        crate::ffi::run_volatile(&format!("EXPLAIN (COSTS OFF) {query}")).unwrap_or_else(|e| error!("{e}"));
    });
    TableIterator::new(decisions.into_iter().map(|d| (d.table, d.mode.to_string(), d.reason, d.advice)))
}
