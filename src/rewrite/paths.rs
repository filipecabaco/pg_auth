//! AuthzBatch's paths and plans: the scans of a relation carrying a pg_auth policy wrapped in a
//! custom scan that checks their rows (batch.rs runs it).
//!
//! Two shapes, both for plain reads:
//! - one child: the relation carries the per-row predicate (too many objects to list). Rows are
//!   checked in batches.
//! - two children: the relation carries the listed-ids filter `col = ANY($n)`. A cached plan can
//!   be reused by a subject whose objects cannot be listed, so the scan decides at execution:
//!   listed subject -> child 0, the scan with the filter, whose rows pass as they are; otherwise
//!   child 1, the same scan without the filter, checked in batches. The filter's InitPlan then
//!   never runs, so the column's values are not all checked.

use super::REPLAN_ON_BREADTH;
use super::guard::{self, Policy, authz_namespace, const_text, match_policy, usable_key};
use crate::pg::plan::{self, CustomPlanning, CustomScan, Relids};
use crate::pg::{List, Node, Oid, Ptr, catalog, pg_sys};
use crate::sidecar::{self, Listed};
use pgrx::pg_sys::NodeTag;
use std::collections::HashMap;
use std::fmt::Write as _;

type Root<'a> = Ptr<'a, pg_sys::PlannerInfo>;
type Rel<'a> = Ptr<'a, pg_sys::RelOptInfo>;
type Path<'a> = Ptr<'a, pg_sys::Path>;
type Restriction<'a> = Ptr<'a, pg_sys::RestrictInfo>;

/// Match the listed-ids filter after planning has turned its sub-select into an InitPlan:
/// `col = ANY($n)` where $n is set by an InitPlan computing `authz.allowed_*('<type>', '<permission>', ...)`,
/// or `col = $n` from `authz.allowed_one_*` (a subject with at most one id).
fn match_listed<'a>(root: Root<'a>, clause: Node<'a>, nsp: Oid) -> Option<Policy<'a>> {
    let (args, single) = if let Some(saop) = clause.cast::<pg_sys::ScalarArrayOpExpr>().filter(|s| s.use_or()) {
        (saop.args(), false)
    } else {
        (clause.cast::<pg_sys::OpExpr>()?.args(), true)
    };
    if args.len() != 2 {
        return None;
    }
    let (var, param) = (args.get(0)?, args.get(1)?.cast::<pg_sys::Param>()?);
    if !usable_key(var) || param.paramkind() != pg_sys::ParamKind::PARAM_EXEC {
        return None;
    }
    let initplan = root.init_plans().iter().find(|sp| sp.set_param().contains(param.paramid()))?;
    let plan = root.glob()?.subplans().get(usize::try_from(initplan.plan_id() - 1).ok()?)?;
    let func = plan.targetlist().get(0)?.expr()?.cast::<pg_sys::FuncExpr>()?;
    let name = catalog::function_name(func.funcid())?;
    if catalog::function_namespace(func.funcid()) != nsp || !name.starts_with("allowed_") || name.starts_with("allowed_one_") != single {
        return None;
    }
    let fargs = func.args();
    Some(Policy { key: var, object_type: const_text(fargs.get(0)?)?, permission: const_text(fargs.get(1)?)?, single, attrs: None })
}

/// The pg_auth policy (per-row, not attributes) or listed-ids filter a clause is.
fn ours<'a>(root: Root<'a>, clause: Node<'a>, nsp: Oid) -> bool {
    match_policy(clause, nsp).is_some() || match_listed(root, clause, nsp).is_some()
}

/// Replace the scans of a relation carrying a pg_auth policy (per-row predicate or listed-ids
/// filter) with AuthzBatch scans over them. Each keeps its child's ordering where it can.
pub fn add_batch_paths<'a>(root: Root<'a>, rel: Rel<'a>, rti: u32, rte: Ptr<'a, pg_sys::RangeTblEntry>) {
    if rte.rtekind() != pg_sys::RTEKind::RTE_RELATION || rte.inh() {
        return;
    }
    let mut r = Some(root);
    while let Some(level) = r {
        if !level.parse().is_some_and(guard::read_only) {
            return;
        }
        r = level.parent_root();
    }
    let Some(nsp) = authz_namespace() else { return };
    let on_rel = |g: &Policy| g.varno() == rti;
    let restrictions = rel.baserestrictinfo();
    // rows reach AuthzBatch as copies (or a child's projection), which carry no system columns
    let mut nodes: Vec<Node> = rel.reltarget().and_then(|t| t.exprs().as_node()).into_iter().collect();
    nodes.extend(restrictions.iter().filter_map(Ptr::clause));
    if catalog::attnos(nodes, rti).iter().any(|a| *a < 0) {
        return;
    }
    let per_row =
        restrictions.iter().find_map(|ri| match_policy(ri.clause()?, nsp).filter(on_rel).filter(|g| g.attrs.is_none()).map(|g| (ri, g)));
    let listed = match per_row {
        Some(_) => None,
        None => restrictions.iter().find_map(|ri| match_listed(root, ri.clause()?, nsp).filter(on_rel).map(|g| (ri, g))),
    };

    // children read only the columns AuthzBatch uses, which an index-only path already covers
    let scans = [
        NodeTag::T_SeqScan,
        NodeTag::T_IndexScan,
        NodeTag::T_IndexOnlyScan,
        NodeTag::T_BitmapHeapScan,
        NodeTag::T_TidScan,
        NodeTag::T_TidRangeScan,
    ];
    let paths: Vec<Path> = rel.pathlist().iter().filter(|p| scans.contains(&p.pathtype())).collect();
    let Some((filter, policy)) = per_row.as_ref().or(listed.as_ref()) else { return };
    let factor = selectivity_factor(root, rti, *filter, policy);
    if !factor.total_cmp(&1.0).is_eq() {
        rel.set_rows(plan::clamp_rows(rel.rows() * factor));
    }
    // A listed subject's scan needs no AuthzBatch around it when cached plans are rebuilt for a
    // subject of another breadth (pg_auth.replan_on_breadth): a subject too broad to list, which
    // the unlisted child was for, gets its own plan. The planner's own paths stay, with the
    // subject's row estimate; only ordered pages add the adaptive path. A one-id plan keeps the
    // wrapper: subjects with a few more ids share its breadth class, and need its fallback.
    if let (None, Some((ri, g))) = (&per_row, &listed)
        && REPLAN_ON_BREADTH.get()
        && !g.single
    {
        // the adaptive path first: it picks its fetching child among the planner's paths as
        // costed for any subject, before they take this subject's estimate
        let adaptive = adaptive_path(root, rel, rti, *ri, g);
        let mut without_list = Vec::new();
        for p in paths {
            p.set_rows(plan::clamp_rows(p.rows() * factor));
            if index_uses(p, *ri) {
                p.set_total_cost(p.startup_cost() + (p.total_cost() - p.startup_cost()) * factor);
                // next to another index condition (the inner side of a nested loop on the
                // same column, a range), the list is better a hashed filter on the rows found:
                // as an index condition, every rescan sorts the whole array again
                if p.cast::<pg_sys::IndexPath>().is_some_and(|ip| ip.indexclauses().len() > 1) {
                    without_list.push(without_listed(root, rel, p, *ri).0);
                }
            }
        }
        for p in without_list {
            plan::add_path(rel, p);
        }
        if let Some(adaptive) = adaptive {
            plan::add_path(rel, adaptive.path());
        }
        return;
    }
    let mut batched: Vec<Ptr<pg_sys::CustomPath>> = match (&per_row, &listed) {
        (Some((_, g)), _) => {
            paths.into_iter().filter_map(|p| Some(estimated(batch_path(rel, &[p], p.pathkeys(), g)?, p, factor, false))).collect()
        }
        (None, Some((ri, g))) => paths
            .into_iter()
            .filter_map(|p| {
                let (unlisted, keys) = if g.single && !p.pathkeys().is_empty() {
                    ordered_unlisted(root, rel, p, *ri, g.column())
                } else {
                    without_listed(root, rel, p, *ri)
                };
                Some(estimated(batch_path(rel, &[p, unlisted], keys, g)?, p, factor, index_uses(p, *ri)))
            })
            .collect(),
        (None, None) => return,
    };
    if let Some((ri, g)) = &listed {
        batched.extend(adaptive_path(root, rel, rti, *ri, g));
    }
    if batched.is_empty() {
        return;
    }
    rel.set_pathlist(List::empty());
    // AuthzBatch is not parallel aware; pg_auth's functions are parallel restricted, which
    // already rules out partial paths, so this only makes that explicit
    rel.set_partial_pathlist(List::empty());
    for p in batched {
        plan::add_path(rel, p.path());
    }
}

/// The scan to use for subjects whose ids are not listed: `path` without the listed-ids
/// filter, and the ordering both children share (the AuthzBatch path's pathkeys).
fn without_listed<'a>(root: Root<'a>, rel: Rel<'a>, path: Path<'a>, listed: Restriction<'a>) -> (Path<'a>, List<'a, pg_sys::Node>) {
    let outer: Option<Relids> = path.param_info().map(Ptr::required_outer);
    let seqscan = || (plan::seqscan_path(root, rel, outer), List::empty());
    match path.pathtype() {
        NodeTag::T_IndexScan | NodeTag::T_IndexOnlyScan if index_uses(path, listed) => {
            let Some(ip) = path.cast::<pg_sys::IndexPath>() else { return seqscan() };
            // the same index without the filter, so the ordering (and other conditions) remain
            let rest: Vec<_> = ip.indexclauses().iter().filter(|ic| ic.rinfo() != Some(listed)).collect();
            if rest.is_empty() && path.pathkeys().is_empty() {
                return seqscan();
            }
            let unlisted = plan::index_path_like(root, ip, rel.memory().list(rest), outer);
            (unlisted, path.pathkeys())
        }
        // bitmap scans are unordered
        NodeTag::T_BitmapHeapScan
            if path.cast::<pg_sys::BitmapHeapPath>().and_then(Ptr::bitmapqual).is_some_and(|q| index_uses(q, listed)) =>
        {
            seqscan()
        }
        // the filter is only a condition on the rows: the same scan serves both
        _ => (path, path.pathkeys()),
    }
}

/// How much of the relation the subject's filter lets through, relative to what the planner
/// assumed. Postgres cannot see into the InitPlan, so it estimates `col = ANY($ids)` as about ten
/// ids whatever the subject's count, and pg_auth's per-row check as a fixed fraction. With the
/// count known while planning (the listed ids, or the size of a fresh permission index set) over
/// the column's distinct values, it weighs walking an ordered index against fetching the
/// subject's rows and sorting them as it would with the ids written out. 1.0 when either is
/// unknown.
#[allow(clippy::cast_precision_loss)] // object counts, far below 2^52
fn selectivity_factor<'a>(root: Root<'a>, rti: u32, filter: Restriction<'a>, g: &Policy<'a>) -> f64 {
    let Some(subject) = crate::subject::current_as_caller() else { return 1.0 };
    let Some((distinct, common)) = plan::column_stats(root, rti, g.key) else { return 1.0 };
    // the subject's share of the rows: its ids' own shares where the statistics have them (a
    // tenant with half the table is not one 2000th of it)
    let share = match sidecar::list(&subject, &g.object_type, &g.permission).as_deref() {
        Ok(Listed::Ids(ids)) => share_of(ids, distinct, &common),
        Ok(Listed::TooMany) => match sidecar::index_status(&subject, &g.object_type, &g.permission) {
            sidecar::IndexStatus::Fresh(n) => n as f64 / distinct,
            _ => return 1.0,
        },
        Err(_) => return 1.0,
    };
    let Some(clause) = filter.clause() else { return 1.0 };
    let assumed = plan::clause_selectivity(root, clause, rti);
    if assumed <= 0.0 {
        return 1.0;
    }
    // only ever up: Postgres's guess already over-counts a narrow subject, and a generic plan
    // cached for one is reused by broader ones, which a smaller estimate would serve worse
    (share.clamp(0.0, 1.0) / assumed).max(1.0)
}

/// The share of the rows `ids` hold: a common value's own share (the column's statistics), an
/// even part of what the common values leave for the others.
#[allow(clippy::cast_precision_loss)] // a statistics target's worth of values
pub fn share_of(ids: &crate::sidecar::IdSet, distinct: f64, common: &HashMap<String, f64>) -> f64 {
    let others = (distinct - common.len() as f64).max(1.0);
    let other = ((1.0 - common.values().sum::<f64>()) / others).max(0.0);
    ids.iter().map(|id| common.get(id).copied().unwrap_or(other)).sum()
}

/// For a single-table read with an ORDER BY, an AuthzBatch path already in that order whose two
/// children suit subjects of any breadth, so a cached plan serves them all: child 0 fetches the
/// subject's rows through its listed-ids filter and sorts them (a narrow subject), child 1 walks
/// an index in that order unfiltered, checking rows until LIMIT is met (a broad one, or one too
/// broad to list). Which one runs is decided at execution (`Adaptive::narrow`), from the
/// subject's id count and the table's statistics recorded here.
fn adaptive_path<'a>(
    root: Root<'a>,
    rel: Rel<'a>,
    rti: u32,
    listed: Restriction<'a>,
    g: &Policy<'a>,
) -> Option<Ptr<'a, pg_sys::CustomPath>> {
    let keys = root.query_pathkeys();
    if keys.is_empty() || root.all_baserels().len() != 1 {
        return None;
    }
    let walk = ordered_walk(root, rel, keys, listed, g.column())?;
    let narrow = rel.pathlist().iter().filter(|p| p.param_info().is_none()).min_by(|a, b| a.total_cost().total_cmp(&b.total_cost()))?;
    let narrow = detached(root, rel, narrow)?;
    let narrow = if plan::pathkeys_contained_in(keys, narrow.pathkeys()) {
        narrow
    } else {
        plan::sort_path(root, rel, narrow, keys, root.limit_tuples())
    };
    let (distinct, common) = plan::column_stats(root, rti, g.key)?;
    // the rows the query's own conditions leave (what the walk reads at most, and what each id
    // stands for when fetched), not the whole table
    let own = rel.memory().list(rel.baserestrictinfo().iter().filter(|ri| *ri != listed));
    let selectivity = plan::clauses_selectivity(root, own, rti);
    let tuples = (rel.tuples() * selectivity).max(1.0);
    let cp = batch_path(rel, &[narrow, walk], keys, g)?;
    // the narrow child's cost, for the subject planned for; a little under an explicit sort of
    // the same rows, so this path wins that tie
    cp.path().set_startup_cost(narrow.startup_cost());
    cp.path().set_total_cost(narrow.total_cost() * 0.999);
    let m = rel.memory();
    // the column's most common values and their shares: a subject's rows are not the average
    // share when its ids are skewed (one tenant with 8% of the rows)
    let common = common.iter().fold(String::new(), |mut text, (v, f)| {
        let _ = write!(text, "{v}\u{1f}{f}\u{1e}");
        text
    });
    let stats = m.string(&format!("{},{},{}", tuples, distinct, root.limit_tuples()));
    cp.set_custom_private(m.list(cp.custom_private().iter().chain([stats, m.string(&common)])));
    Some(cp)
}

/// `path`, or the same scan built again, as a child no `add_path` on the relation can free:
/// add_path frees every path a new one dominates except index paths, and the relation's paths
/// stay in its pathlist when a listed scan is not wrapped (an ordered path cheaper than this
/// subject's raised estimate of a bitmap scan dominates it). The copy keeps the original's
/// estimate and costs. None for other kinds of scan.
fn detached<'a>(root: Root<'a>, rel: Rel<'a>, path: Path<'a>) -> Option<Path<'a>> {
    let copy = match path.pathtype() {
        NodeTag::T_IndexScan | NodeTag::T_IndexOnlyScan => return Some(path),
        NodeTag::T_SeqScan => plan::seqscan_path(root, rel, None),
        NodeTag::T_BitmapHeapScan => plan::bitmap_heap_path(root, rel, path.cast::<pg_sys::BitmapHeapPath>()?.bitmapqual()?),
        _ => return None,
    };
    copy.set_rows(path.rows());
    copy.set_startup_cost(path.startup_cost());
    copy.set_total_cost(path.total_cost());
    Some(copy)
}

/// An unparameterized scan of the relation in the order `keys` without the listed-ids filter, on
/// an index that does not involve `column` (whose equality could be where an index on it gets
/// the order from). The query's own conditions stay index conditions where the index serves them
/// (a name range on (bucket_id, name)), so the walk reads that range, not the whole index.
fn ordered_walk<'a>(root: Root<'a>, rel: Rel<'a>, keys: List<'a, pg_sys::Node>, listed: Restriction<'a>, column: i16) -> Option<Path<'a>> {
    unlisted_index_paths(root, rel, listed, column)
        .into_iter()
        .filter(|p| {
            p.pathtype() == NodeTag::T_IndexScan
                && p.param_info().is_none()
                && plan::pathkeys_contained_in(keys, p.pathkeys())
                && p.cast::<pg_sys::IndexPath>().and_then(Ptr::indexinfo).is_some_and(|index| !has_column(index, column))
        })
        .min_by(|a, b| a.total_cost().total_cmp(&b.total_cost()))
}

/// The index paths Postgres builds for `rel` without the listed-ids filter: create_index_paths,
/// with the filter out of the relation's restrictions for the call (and its paths set aside).
/// Only indexes not involving `column` take part: an equality on it (the one-id form) is an
/// equivalence class the planner would still apply, and its cheaper paths would prune the rest.
/// Index-only scans may not read the protected column without that filter: AuthzBatch's callers
/// keep plain index scans.
fn unlisted_index_paths<'a>(root: Root<'a>, rel: Rel<'a>, listed: Restriction<'a>, column: i16) -> Vec<Path<'a>> {
    let (paths, partial, restrictions, indexes) = (rel.pathlist(), rel.partial_pathlist(), rel.baserestrictinfo(), rel.indexlist());
    let m = rel.memory();
    rel.set_indexlist(m.list(indexes.iter().filter(|index| !has_column(*index, column))));
    rel.set_baserestrictinfo(m.list(restrictions.iter().filter(|ri| *ri != listed)));
    rel.set_pathlist(List::empty());
    rel.set_partial_pathlist(List::empty());
    plan::create_index_paths(root, rel);
    let built = rel.pathlist().iter().collect();
    rel.set_pathlist(paths);
    rel.set_partial_pathlist(partial);
    rel.set_baserestrictinfo(restrictions);
    rel.set_indexlist(indexes);
    built
}

/// An AuthzBatch path with the subject's row estimate: `factor` times its child's, and, where the
/// filter is an index condition, that much more (or less) of the index and table read.
fn estimated<'a>(cp: Ptr<'a, pg_sys::CustomPath>, child: Path<'a>, factor: f64, index_condition: bool) -> Ptr<'a, pg_sys::CustomPath> {
    let p = cp.path();
    p.set_rows(plan::clamp_rows(child.rows() * factor));
    if index_condition {
        p.set_total_cost(p.startup_cost() + (p.total_cost() - p.startup_cost()) * factor);
    }
    cp
}

/// The scan for subjects with more than one id when the listed scan is `col = $one`, ordered. Its
/// order may come from that equality (an index on (col, x) gives x's order only for one col
/// value), so the scan without it is not ordered the same. Preferably another scan of the
/// relation in that order that does not get it from the column (an index on x: AuthzBatch keeps
/// its order, and LIMIT stops it early); else this scan without the filter, keeping its
/// parameterization (the inner side of a nested loop), sorted, with every column of the relation
/// (the rows AuthzBatch checks have its row type).
fn ordered_unlisted<'a>(
    root: Root<'a>,
    rel: Rel<'a>,
    path: Path<'a>,
    listed: Restriction<'a>,
    column: i16,
) -> (Path<'a>, List<'a, pg_sys::Node>) {
    let keys = path.pathkeys();
    // the planner kept only the cheapest path per order, for the subject it planned for: build
    // the unfiltered index scan in this order (an unparameterized scan only)
    if path.param_info().is_none()
        && let Some(walk) = ordered_walk(root, rel, keys, listed, column)
    {
        return (walk, keys);
    }
    let (unlisted, _) = without_listed(root, rel, path, listed);
    let projected = plan::projection_path(root, rel, unlisted, plan::every_column(root, rel, None));
    (plan::sort_path(root, rel, projected, keys, -1.0), keys)
}

/// Does `index` have `column` as a key (so its order may come from the column's equality)?
fn has_column(index: Ptr<'_, pg_sys::IndexOptInfo>, column: i16) -> bool {
    index.key_columns().contains(&i32::from(column))
}

/// Does an index path (or bitmap tree of them) use `rinfo` as an index condition?
fn index_uses(path: Path<'_>, rinfo: Restriction<'_>) -> bool {
    if let Some(ip) = path.cast::<pg_sys::IndexPath>() {
        ip.indexclauses().iter().any(|ic| ic.rinfo() == Some(rinfo))
    } else if let Some(heap) = path.cast::<pg_sys::BitmapHeapPath>() {
        heap.bitmapqual().is_some_and(|q| index_uses(q, rinfo))
    } else if let Some(and) = path.cast::<pg_sys::BitmapAndPath>() {
        and.bitmapquals().iter().any(|p| index_uses(p, rinfo))
    } else if let Some(or) = path.cast::<pg_sys::BitmapOrPath>() {
        or.bitmapquals().iter().any(|p| index_uses(p, rinfo))
    } else {
        false
    }
}

/// An AuthzBatch path over `children`, its private data [type, permission, column, single, key].
fn batch_path<'a>(
    rel: Rel<'a>,
    children: &[Path<'a>],
    pathkeys: List<'a, pg_sys::Node>,
    policy: &Policy<'a>,
) -> Option<Ptr<'a, pg_sys::CustomPath>> {
    let m = rel.memory();
    let private = m.list([
        m.string(&policy.object_type),
        m.string(&policy.permission),
        m.integer(i32::from(policy.column())),
        m.integer(i32::from(policy.single)),
        // the key itself, which plan_batch moves into custom_exprs (where Postgres fixes its columns)
        m.copy(policy.key),
    ]);
    // a listed child passes its rows through as AuthzBatch's output, so that output must stay the
    // target list it was planned with: not projection capable, the planner adds a Result node
    // rather than replace it; a batch-only scan projects at execution, from its final target list
    plan::custom_path(rel, children, pathkeys, children.len() != 2, private)
}

/// The attribute numbers `nodes` read from relation `varno`, or None when they read the whole
/// row. System columns are negative.
fn columns_used<'a>(nodes: impl IntoIterator<Item = Node<'a>>, varno: u32) -> Option<Vec<i16>> {
    let attnos = catalog::attnos(nodes, varno);
    if attnos.contains(&0) {
        return None;
    }
    Some(attnos.into_iter().filter_map(|a| i16::try_from(a).ok()).collect())
}

/// The scan under a child: itself, or what the sorted fallback sorts.
fn scan_below(mut plan: Ptr<'_, pg_sys::Plan>) -> Ptr<'_, pg_sys::Plan> {
    while matches!(plan.tag(), NodeTag::T_Sort | NodeTag::T_Result) {
        let Some(below) = plan.lefttree() else { break };
        plan = below;
    }
    plan
}

/// Plan: children return every column and evaluate only leakproof conditions on the table
/// itself, which are safe on rows not checked yet; AuthzBatch evaluates everything else after
/// the check. pg_auth's own condition is dropped where the batch check replaces it: the per-row
/// predicate always, the listed-ids filter in the unlisted child.
pub fn plan_batch(CustomPlanning { root, rel, path, tlist, clauses, children }: CustomPlanning<'_>) -> Ptr<'_, pg_sys::Plan> {
    let m = rel.memory();
    let nsp = authz_namespace().unwrap_or(pg_sys::InvalidOid);

    // AuthzBatch evaluates, after the check, every condition that is not safe on unchecked rows
    let after: Vec<Node> = clauses
        .iter()
        .filter_map(|ri| {
            let node = ri.clause()?;
            // pg_auth's own conditions are what the check replaces (a key expression's may not be
            // leakproof, which would otherwise land them here)
            if ri.pseudoconstant() || ours(root, node, nsp) {
                return None;
            }
            let local = ri.required_relids().is_subset_of(rel.relids());
            (!local || catalog::may_leak(node)).then_some(node)
        })
        .collect();
    let after = m.list(after);
    // each child keeps its own filter, which the planner built without the conditions its index
    // already enforces, minus what moved after the check: pg_auth's per-row predicate, anything
    // not leakproof (join conditions arrive here as nestloop params), and in the unlisted child
    // the listed-ids filter
    let two = children.len() == 2;
    let before: Vec<List<pg_sys::Node>> = children
        .iter()
        .enumerate()
        .map(|(i, child)| {
            let unlisted = two && i == 1;
            m.list(scan_below(child).qual().iter().filter(|e| {
                match_policy(*e, nsp).is_none() && !(unlisted && match_listed(root, *e, nsp).is_some()) && !catalog::may_leak(*e)
            }))
        })
        .collect();
    // what AuthzBatch reads from the rows: its output, its conditions, and the id it checks. The
    // output is the relation's target: `tlist` alone may be empty or partial, when the planner
    // will put a projection on AuthzBatch afterwards (CP_IGNORE_TLIST)
    let private = path.custom_private();
    let attno = private.get(2).and_then(Node::cast::<pg_sys::Integer>).map_or(0, |i| i16::try_from(i.value()).unwrap_or(0));
    let key = private.get(4);
    let tlist_node = tlist.nodes().as_node();
    let target = rel.reltarget().and_then(|t| t.exprs().as_node());
    let used = columns_used([tlist_node, target, after.as_node(), key].into_iter().flatten(), rel.relid()).map(|mut c| {
        if attno > 0 {
            c.push(attno);
        }
        c
    });
    for (i, (child, quals)) in children.iter().zip(before).enumerate() {
        if two && i == 0 {
            // the listed child's rows are authorized by its listed-ids filter, and pass through
            // AuthzBatch as they are: it keeps the planner's own conditions (in RLS's security
            // order) and produces AuthzBatch's output itself, so pass-through rows cost nothing
            let output = tlist.copy(m);
            if child.tag() == NodeTag::T_Sort {
                // a sort's target list holds its keys: a Result above it produces the output
                children.set(0, plan::result_over(child, output));
            } else {
                child.set_targetlist(output);
            }
            continue;
        }
        if child.tag() == NodeTag::T_Sort {
            // the sorted fallback (ordered_unlisted): it already reads every column, and its sort
            // keys point into that target list
            scan_below(child).set_qual(quals);
            continue;
        }
        child.set_targetlist(plan::every_column(root, rel, used.as_deref()));
        child.set_qual(quals);
    }

    // [type, permission, column, single, key, adaptive stats, common values, constant]
    let mut private: Vec<Node> = private.iter().collect();
    while private.len() < 7 {
        private.push(m.string(""));
    }
    private.push(m.string(&pinned_value(rel, attno).unwrap_or_default()));
    // a key expression, evaluated on each row: here Postgres fixes its columns like any plan
    // expression's
    let exprs = if attno <= 0 { m.list(key.map(|k| m.copy(k))) } else { List::empty() };
    CustomScan { tlist, qual: after, scanrelid: rel.relid(), flags: path.flags(), children, private: m.list(private), exprs }.build()
}

/// The one value the query's own conditions pin the protected column to (`tenant_id = 1`), as
/// ids are listed: then every row's id is that value, and one check answers for all of them.
/// Only for types whose equality is identity of the text form (integers, uuid, text in a
/// deterministic collation).
fn pinned_value(rel: Rel<'_>, attno: i16) -> Option<String> {
    if attno <= 0 {
        return None;
    }
    rel.baserestrictinfo().iter().find_map(|ri| {
        let op = ri.clause()?.cast::<pg_sys::OpExpr>()?;
        let (a, b) = (op.args().get(0)?, op.args().get(1)?);
        let ((Some(var), Some(value), _, _) | (_, _, Some(var), Some(value))) =
            (a.cast::<pg_sys::Var>(), b.cast::<pg_sys::Const>(), b.cast::<pg_sys::Var>(), a.cast::<pg_sys::Const>())
        else {
            return None;
        };
        if u32::try_from(var.varno()).ok() != Some(rel.relid()) || var.varattno() != attno || var.varlevelsup() != 0 || value.is_null() {
            return None;
        }
        let typ = var.vartype();
        let exact = matches!(typ, pg_sys::INT2OID | pg_sys::INT4OID | pg_sys::INT8OID | pg_sys::UUIDOID)
            || (typ == pg_sys::TEXTOID && catalog::collation_is_deterministic(op.input_collid()));
        if !exact || value.const_type() != typ || op.opno() != catalog::equality(typ) {
            return None;
        }
        Some(catalog::output(typ, value.value()))
    })
}
