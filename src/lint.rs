//! `authz.lint_policies()`: static analysis of every row level security policy in the database.
//!
//! Each policy's USING / WITH CHECK expression is read as text (`pg_node_tree`) and parsed with
//! `nodetree`, so the analysis never touches Postgres memory. It looks for the patterns in
//! docs/research/rls-issues.md that make RLS slow:
//! - functions that run once per row;
//! - row-independent calls (auth.uid(), current_setting) that are not hoisted into an InitPlan;
//! - correlated subqueries (one SubPlan run per row);
//! - policies that read tables with their own policies;
//! - filter columns without an index.
//!
//! Catalog-level checks (partitions, views, missing policies) live in sql/lint.sql.

use crate::nodetree::{self, Node, Value};
use pgrx::pg_sys::Oid;
use pgrx::prelude::*;
use std::collections::BTreeSet;

// SubLinkType values as serialized (src/include/nodes/primnodes.h).
const EXISTS_SUBLINK: i64 = 0;
const ANY_SUBLINK: i64 = 2;
// RTEKind::RTE_RELATION
const RTE_RELATION: i64 = 0;

type Finding = (String, String, String, String, String);

#[pg_extern(stable)]
fn lint_policies() -> TableIterator<
    'static,
    (name!(severity, String), name!(code, String), name!(object, String), name!(message, String), name!(hint, String)),
> {
    let policies: Vec<(Oid, String, Option<String>, Option<String>)> = Spi::connect(|client| {
        client
            .select(
                "SELECT p.polrelid, format('%s.%I', p.polrelid::regclass, p.polname),
                        p.polqual::text, p.polwithcheck::text
                 FROM pg_policy p
                 JOIN pg_class c ON c.oid = p.polrelid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
                 ORDER BY 2",
                None,
                &[],
            )
            .map(|rows| {
                rows.filter_map(|r| {
                    Some((r.get::<Oid>(1).ok()??, r.get::<String>(2).ok()??, r.get::<String>(3).ok()?, r.get::<String>(4).ok()?))
                })
                .collect()
            })
            .unwrap_or_default()
    });

    let mut rows = Vec::new();
    for (relid, name, qual, check) in policies {
        let mut analysis = Analysis::default();
        for expr in [qual, check].into_iter().flatten() {
            match nodetree::parse(&expr) {
                Ok(tree) => {
                    let mut ctx = Ctx { out: &mut analysis, policy_rel: relid, queries: Vec::new(), correlated: false };
                    walk(&tree, &mut ctx);
                }
                Err(e) => rows.push(finding("info", "P000", &name, format!("could not analyse policy: {e}"), "")),
            }
        }
        report(&name, &analysis, &mut rows);
    }
    TableIterator::new(rows)
}

#[derive(Default)]
struct Analysis {
    /// Function calls that run once per row: (funcid, references a row).
    per_row_calls: Vec<(Oid, bool)>,
    correlated_subqueries: usize,
    /// Tables read by subqueries inside the policy.
    subquery_rels: BTreeSet<u32>,
    /// Columns compared against a value that does not depend on that table's row.
    filter_columns: BTreeSet<(u32, i64)>,
}

struct Ctx<'a, 'b> {
    out: &'a mut Analysis,
    policy_rel: Oid,
    /// Subqueries entered so far; the last one owns level-0 Vars inside it.
    queries: Vec<&'b Node>,
    /// Inside a subquery that references the policy row, so it runs once per row.
    correlated: bool,
}

fn walk<'b>(value: &'b Value, ctx: &mut Ctx<'_, 'b>) {
    let Some(node) = value.node() else {
        for child in value.children() {
            walk(child, ctx);
        }
        return;
    };
    let per_row = ctx.queries.is_empty() || ctx.correlated;

    match node.tag.as_str() {
        "FUNCEXPR" if per_row => {
            // Depends on the policy row (level = subquery depth) or on the current subquery row.
            let uses_row = refs_level(value, i64::try_from(ctx.queries.len()).unwrap_or(i64::MAX)) || refs_level(value, 0);
            if let Some(funcid) = node.int("funcid").and_then(|f| u32::try_from(f).ok()) {
                ctx.out.per_row_calls.push((Oid::from(funcid), uses_row));
            }
            if !uses_row {
                // A row-independent call is reported once; its arguments add nothing.
                return;
            }
        }
        "OPEXPR" | "SCALARARRAYOPEXPR" => record_filter_column(node, ctx),
        "SUBLINK" => {
            let Some(query_value) = node.get("subselect") else { return };
            let Some(query) = query_value.node() else { return };
            // Vars one level up inside the subquery refer to the row being checked.
            let correlated = refs_level(query_value, 0);
            let kind = node.int("subLinkType").unwrap_or(-1);
            if ctx.queries.is_empty() && correlated && (kind == EXISTS_SUBLINK || kind == ANY_SUBLINK) {
                ctx.out.correlated_subqueries += 1;
            }
            if let Some(test) = node.get("testexpr") {
                walk(test, ctx);
            }

            for rte in query.get("rtable").map(Value::items).unwrap_or_default() {
                if let Some(rte) = rte.node()
                    && rte.int("rtekind") == Some(RTE_RELATION)
                    && let Some(relid) = rte.int("relid").and_then(|r| u32::try_from(r).ok())
                {
                    ctx.out.subquery_rels.insert(relid);
                }
            }

            let saved = ctx.correlated;
            ctx.correlated = saved || correlated;
            ctx.queries.push(query);
            for field in ["jointree", "targetList"] {
                if let Some(v) = query.get(field) {
                    walk(v, ctx);
                }
            }
            ctx.queries.pop();
            ctx.correlated = saved;
            return;
        }
        _ => {}
    }
    for child in value.children() {
        walk(child, ctx);
    }
}

/// Does `value` contain a Var that is `level` query levels up from where `value` sits?
/// Entering a nested Query moves every Var one level further away.
fn refs_level(value: &Value, level: i64) -> bool {
    match value.node() {
        Some(n) if n.tag == "VAR" => n.int("varlevelsup") == Some(level),
        Some(n) if n.tag == "QUERY" => value.children().into_iter().any(|c| refs_level(c, level + 1)),
        _ => value.children().into_iter().any(|c| refs_level(c, level)),
    }
}

/// `col <op> value` where value does not depend on col's row: col should be indexed.
fn record_filter_column(node: &Node, ctx: &mut Ctx) {
    let args = node.get("args").map(Value::items).unwrap_or_default();
    if args.len() != 2 {
        return;
    }
    for (this, other) in [(0, 1), (1, 0)] {
        let var = strip_relabel(&args[this]);
        let Some(var) = var.node().filter(|n| n.tag == "VAR" && n.int("varlevelsup") == Some(0)) else {
            continue;
        };
        if refs_level(&args[other], 0) {
            continue;
        }
        if let (Some(relid), Some(attno)) = (var_relation(ctx, var), var.int("varattno")) {
            ctx.out.filter_columns.insert((relid, attno));
        }
    }
}

fn var_relation(ctx: &Ctx, var: &Node) -> Option<u32> {
    match ctx.queries.last() {
        None => Some(u32::from(ctx.policy_rel)),
        Some(query) => {
            let varno = var.int("varno")?;
            let rte = query.get("rtable")?.items().get(usize::try_from(varno - 1).ok()?)?.node()?;
            if rte.int("rtekind") == Some(RTE_RELATION) { rte.int("relid").and_then(|r| u32::try_from(r).ok()) } else { None }
        }
    }
}

fn strip_relabel(mut value: &Value) -> &Value {
    while let Some(n) = value.node().filter(|n| n.tag == "RELABELTYPE") {
        match n.get("arg") {
            Some(arg) => value = arg,
            None => break,
        }
    }
    value
}

fn finding(severity: &str, code: &str, object: &str, message: String, hint: &str) -> Finding {
    (severity.into(), code.into(), object.into(), message, hint.into())
}

fn report(policy: &str, a: &Analysis, out: &mut Vec<Finding>) {
    let mut seen = BTreeSet::new();
    for &(funcid, uses_row) in &a.per_row_calls {
        if !seen.insert((u32::from(funcid), uses_row)) {
            continue;
        }
        let Some((name, lang, secdef, has_config, volatility)) = func_info(funcid) else { continue };
        let inlinable = lang == "sql" && !secdef && !has_config;
        if uses_row && !inlinable && lang != "internal" && lang != "c" {
            out.push(finding(
                "high",
                "P001",
                policy,
                format!("{name} ({lang}{}) runs once for every row", if secdef { ", security definer" } else { "" }),
                "compute the allowed set once, e.g. col = ANY ((select array(...))), or protect the table with authz.bind()",
            ));
        } else if !uses_row && volatility != "i" {
            let jwt = name.starts_with("current_setting") || name.contains("jwt") || name.contains("uid");
            out.push(finding(
                if lang == "plpgsql" { "high" } else { "medium" },
                "P002",
                policy,
                format!(
                    "{name} does not depend on the row but is evaluated for every row{}",
                    if jwt { " (request claims are re-read and re-parsed each time)" } else { "" }
                ),
                "wrap the call in a scalar subquery so it runs once as an InitPlan: (select fn())",
            ));
        }
    }

    if a.correlated_subqueries > 0 {
        out.push(finding(
            "high",
            "P003",
            policy,
            format!("{} correlated subquery(ies) re-run for every row (SubPlan)", a.correlated_subqueries),
            "make the subquery independent of the row: col IN (select ... where user_id = (select auth.uid())) \
             or col = ANY ((select array(...)))",
        ));
    }

    for &relid in &a.subquery_rels {
        if let Some(name) = rls_table_name(Oid::from(relid)) {
            out.push(finding(
                "medium",
                "P004",
                policy,
                format!("reads {name}, which has row level security: its policies run inside this one"),
                "read it through a SECURITY DEFINER helper that returns the allowed ids, or precompute with pg_auth",
            ));
        }
    }

    for &(relid, attno) in &a.filter_columns {
        if attno > 0
            && let Some(col) = i16::try_from(attno).ok().and_then(|attno| unindexed_column(Oid::from(relid), attno))
        {
            out.push(finding(
                "medium",
                "P005",
                policy,
                format!("filters on {col}, which no index starts with"),
                "create an index whose first column is that column",
            ));
        }
    }
}

fn func_info(funcid: Oid) -> Option<(String, String, bool, bool, String)> {
    Spi::connect(|client| {
        let mut rows = client
            .select(
                "SELECT p.oid::regprocedure::text, l.lanname::text, p.prosecdef,
                        p.proconfig IS NOT NULL, p.provolatile::text
                 FROM pg_proc p JOIN pg_language l ON l.oid = p.prolang
                 WHERE p.oid = $1",
                None,
                &[funcid.into()],
            )
            .ok()?;
        let r = rows.next()?;
        Some((
            r.get::<String>(1).ok()??,
            r.get::<String>(2).ok()??,
            r.get::<bool>(3).ok()??,
            r.get::<bool>(4).ok()??,
            r.get::<String>(5).ok()??,
        ))
    })
}

fn rls_table_name(relid: Oid) -> Option<String> {
    crate::ffi::read_one::<String>("SELECT oid::regclass::text FROM pg_class WHERE oid = $1 AND relrowsecurity", &[relid.into()])
}

fn unindexed_column(relid: Oid, attno: i16) -> Option<String> {
    crate::ffi::read_one::<String>(
        "SELECT format('%s.%I', c.oid::regclass, a.attname)
         FROM pg_class c JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = $2
         WHERE c.oid = $1 AND c.relkind IN ('r', 'p', 'm')
           AND NOT EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indkey[0] = $2)",
        &[relid.into(), attno.into()],
    )
}
