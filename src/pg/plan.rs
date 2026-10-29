//! The planner's side: handles on planner nodes (paths, relations, restrictions, plans), the
//! planner functions pg_auth calls on them, the set_rel_pathlist hook, and AuthzBatch's custom
//! path and plan.

use super::{Field, List, Memory, Node, NodeStruct, Ptr, Tagged, catalog, fields, pg_sys};
use pgrx::pg_guard;
use pgrx::pg_sys::NodeTag;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_int};
use std::rc::Rc;
use std::sync::OnceLock;

/// A set of relations (a Bitmapset of range table indexes), as the planner passes it around.
#[derive(Clone, Copy)]
pub struct Relids<'a>(*mut pg_sys::Bitmapset, std::marker::PhantomData<&'a ()>);

// SAFETY: a Relids field holds a live bitmapset, or null (the empty set)
unsafe impl<'a> Field<'a, *mut pg_sys::Bitmapset> for Relids<'a> {
    unsafe fn read(raw: *mut pg_sys::Bitmapset) -> Self {
        Relids(raw, std::marker::PhantomData)
    }
    fn write(self) -> *mut pg_sys::Bitmapset {
        self.0
    }
}

impl Relids<'_> {
    pub fn is_subset_of(self, other: Self) -> bool {
        // SAFETY: two live bitmapsets (or null)
        unsafe { pg_sys::bms_is_subset(self.0, other.0) }
    }

    pub fn len(self) -> usize {
        // SAFETY: a live bitmapset (or null)
        usize::try_from(unsafe { pg_sys::bms_num_members(self.0) }).unwrap_or(0)
    }
}

/// Structs that begin with a `Path` (every kind of path).
///
/// # Safety
/// The struct's first field is a `Path`.
pub unsafe trait PathStruct: NodeStruct {}

// SAFETY: these begin with their Path (pathnodes.h)
unsafe impl PathStruct for pg_sys::Path {}
// SAFETY: as above
unsafe impl PathStruct for pg_sys::BitmapAndPath {}
// SAFETY: as above
unsafe impl PathStruct for pg_sys::BitmapOrPath {}
// SAFETY: as above
unsafe impl PathStruct for pg_sys::IndexPath {}
// SAFETY: as above
unsafe impl PathStruct for pg_sys::BitmapHeapPath {}
// SAFETY: as above
unsafe impl PathStruct for pg_sys::CustomPath {}

impl<'a, T: PathStruct> Ptr<'a, T> {
    /// This path as a plain `Path`.
    pub fn path(self) -> Ptr<'a, pg_sys::Path> {
        self.retype()
    }
}

impl<'a> Ptr<'a, pg_sys::Path> {
    /// This path as a `T`, when its node tag says it is one.
    pub fn cast<T: Tagged + PathStruct>(self) -> Option<Ptr<'a, T>> {
        self.node().cast()
    }
}

impl Ptr<'_, pg_sys::Plan> {
    pub fn tag(self) -> NodeTag {
        self.node().tag()
    }
}

fields!(PlannerInfo {
    parse: parse -> Option<Ptr<'a, pg_sys::Query>>,
    parent_root: parent_root -> Option<Ptr<'a, pg_sys::PlannerInfo>>,
    init_plans: init_plans -> List<'a, pg_sys::SubPlan>,
    glob: glob -> Option<Ptr<'a, pg_sys::PlannerGlobal>>,
    query_pathkeys: query_pathkeys -> List<'a, pg_sys::Node>,
    all_baserels: all_baserels -> Relids<'a>,
    limit_tuples: limit_tuples -> f64,
});
fields!(PlannerGlobal { subplans: subplans -> List<'a, pg_sys::Plan> });
fields!(RelOptInfo {
    relid: relid -> u32,
    relids: relids -> Relids<'a>,
    rows / set_rows: rows -> f64,
    tuples: tuples -> f64,
    reltarget: reltarget -> Option<Ptr<'a, pg_sys::PathTarget>>,
    pathlist / set_pathlist: pathlist -> List<'a, pg_sys::Path>,
    partial_pathlist / set_partial_pathlist: partial_pathlist -> List<'a, pg_sys::Path>,
    baserestrictinfo / set_baserestrictinfo: baserestrictinfo -> List<'a, pg_sys::RestrictInfo>,
    indexlist / set_indexlist: indexlist -> List<'a, pg_sys::IndexOptInfo>,
});
fields!(PathTarget { exprs: exprs -> List<'a, pg_sys::Node> });
fields!(RestrictInfo { clause: clause -> Option<Node<'a>>, pseudoconstant: pseudoconstant -> bool, required_relids: required_relids -> Relids<'a> });
fields!(Path {
    pathtype: pathtype -> NodeTag,
    param_info: param_info -> Option<Ptr<'a, pg_sys::ParamPathInfo>>,
    rows / set_rows: rows -> f64,
    startup_cost / set_startup_cost: startup_cost -> f64,
    total_cost / set_total_cost: total_cost -> f64,
    pathkeys: pathkeys -> List<'a, pg_sys::Node>,
});
fields!(ParamPathInfo { required_outer: ppi_req_outer -> Relids<'a> });
fields!(IndexPath { indexclauses: indexclauses -> List<'a, pg_sys::IndexClause>, indexinfo: indexinfo -> Option<Ptr<'a, pg_sys::IndexOptInfo>> });
fields!(IndexClause { rinfo: rinfo -> Option<Ptr<'a, pg_sys::RestrictInfo>> });
fields!(BitmapHeapPath { bitmapqual: bitmapqual -> Option<Ptr<'a, pg_sys::Path>> });
fields!(BitmapAndPath { bitmapquals: bitmapquals -> List<'a, pg_sys::Path> });
fields!(BitmapOrPath { bitmapquals: bitmapquals -> List<'a, pg_sys::Path> });
fields!(CustomPath { flags: flags -> u32, custom_private / set_custom_private: custom_private -> List<'a, pg_sys::Node> });
fields!(Plan {
    targetlist / set_targetlist: targetlist -> List<'a, pg_sys::TargetEntry>,
    qual / set_qual: qual -> List<'a, pg_sys::Node>,
    lefttree: lefttree -> Option<Ptr<'a, pg_sys::Plan>>,
});

impl Ptr<'_, pg_sys::IndexOptInfo> {
    /// The table columns of the index's key columns (0 for an expression).
    pub fn key_columns(self) -> Vec<i32> {
        // SAFETY: a live IndexOptInfo, whose indexkeys has nkeycolumns entries
        unsafe {
            let raw = self.as_raw();
            let n = usize::try_from((*raw).nkeycolumns).unwrap_or(0);
            (0..n).map(|i| *(*raw).indexkeys.add(i)).collect()
        }
    }
}

impl<'a> Ptr<'a, pg_sys::PlannerInfo> {
    /// The range table entry of relation `rti`.
    pub fn rte(self, rti: u32) -> Option<Ptr<'a, pg_sys::RangeTblEntry>> {
        let i = usize::try_from(rti).ok()?;
        // SAFETY: a live PlannerInfo's simple_rte_array has an entry for each of its relations;
        // rti comes from one of its RelOptInfos
        unsafe { Ptr::from_raw(*(*self.as_raw()).simple_rte_array.add(i)) }
    }
}

impl<'a> List<'a, pg_sys::Node> {
    /// The list as a node (a List is one), for functions that walk expressions; None when empty.
    pub fn as_node(self) -> Option<Node<'a>> {
        // SAFETY: a non-null List is a node (T_List), live as the list is
        unsafe { Ptr::from_raw(self.as_raw().cast::<pg_sys::Node>()) }
    }
}

impl<'a, T: NodeStruct> List<'a, T> {
    /// A copy of the list and its elements (copyObject).
    pub fn copy(self, _memory: Memory<'a>) -> List<'a, T> {
        // SAFETY: copies a live list of nodes, elements included, into the current context
        unsafe { List::from_raw(pg_sys::copyObjectImpl(self.as_raw().cast()).cast()) }
    }
}

/// `child`'s row estimate, at least one row (clamp_row_est).
pub fn clamp_rows(rows: f64) -> f64 {
    // SAFETY: pure arithmetic
    unsafe { pg_sys::clamp_row_est(rows) }
}

pub fn add_path<'a>(rel: Ptr<'a, pg_sys::RelOptInfo>, path: Ptr<'a, pg_sys::Path>) {
    // SAFETY: adds a live path of this planning to a live relation (add_path may free paths it
    // dominates, except index paths: callers keep no handle on those, see rewrite's `detached`)
    unsafe { pg_sys::add_path(rel.as_raw(), path.as_raw()) }
}

pub fn seqscan_path<'a>(
    root: Ptr<'a, pg_sys::PlannerInfo>,
    rel: Ptr<'a, pg_sys::RelOptInfo>,
    outer: Option<Relids<'a>>,
) -> Ptr<'a, pg_sys::Path> {
    // SAFETY: builds a path for a live relation of this planning
    let raw = unsafe { pg_sys::create_seqscan_path(root.as_raw(), rel.as_raw(), outer.map_or(std::ptr::null_mut(), |r| r.0), 0) };
    made(raw)
}

pub fn bitmap_heap_path<'a>(
    root: Ptr<'a, pg_sys::PlannerInfo>,
    rel: Ptr<'a, pg_sys::RelOptInfo>,
    bitmap: Ptr<'a, pg_sys::Path>,
) -> Ptr<'a, pg_sys::Path> {
    // SAFETY: as above, over a live bitmap path of the relation
    let raw = unsafe { pg_sys::create_bitmap_heap_path(root.as_raw(), rel.as_raw(), bitmap.as_raw(), std::ptr::null_mut(), 1.0, 0) };
    made(raw.cast())
}

/// The same index scan as `path` with `clauses` as its index conditions.
pub fn index_path_like<'a>(
    root: Ptr<'a, pg_sys::PlannerInfo>,
    path: Ptr<'a, pg_sys::IndexPath>,
    clauses: List<'a, pg_sys::IndexClause>,
    outer: Option<Relids<'a>>,
) -> Ptr<'a, pg_sys::Path> {
    let p = path.path();
    // SAFETY: a live index path's fields, passed back to create_index_path as the planner does
    let raw = unsafe {
        let ip = path.as_raw();
        pg_sys::create_index_path(
            root.as_raw(),
            (*ip).indexinfo,
            clauses.as_raw(),
            (*ip).indexorderbys,
            (*ip).indexorderbycols,
            p.pathkeys().as_raw(),
            (*ip).indexscandir,
            p.pathtype() == NodeTag::T_IndexOnlyScan,
            outer.map_or(std::ptr::null_mut(), |r| r.0),
            1.0,
            false,
        )
    };
    made(raw.cast())
}

pub fn sort_path<'a>(
    root: Ptr<'a, pg_sys::PlannerInfo>,
    rel: Ptr<'a, pg_sys::RelOptInfo>,
    path: Ptr<'a, pg_sys::Path>,
    keys: List<'a, pg_sys::Node>,
    limit: f64,
) -> Ptr<'a, pg_sys::Path> {
    // SAFETY: a sort over a live path of the relation, by live pathkeys
    let raw = unsafe { pg_sys::create_sort_path(root.as_raw(), rel.as_raw(), path.as_raw(), keys.as_raw(), limit) };
    made(raw.cast())
}

/// `path` projected to `targets` (a target list).
pub fn projection_path<'a>(
    root: Ptr<'a, pg_sys::PlannerInfo>,
    rel: Ptr<'a, pg_sys::RelOptInfo>,
    path: Ptr<'a, pg_sys::Path>,
    targets: List<'a, pg_sys::TargetEntry>,
) -> Ptr<'a, pg_sys::Path> {
    // SAFETY: a projection of a live path to a target built from a live target list
    let raw = unsafe {
        let target = pg_sys::make_pathtarget_from_tlist(targets.as_raw());
        pg_sys::create_projection_path(root.as_raw(), rel.as_raw(), path.as_raw(), target)
    };
    made(raw.cast())
}

/// Postgres's index paths for `rel` as it stands (create_index_paths), added to its pathlist.
pub fn create_index_paths<'a>(root: Ptr<'a, pg_sys::PlannerInfo>, rel: Ptr<'a, pg_sys::RelOptInfo>) {
    // SAFETY: builds paths for a live relation of this planning
    unsafe { pg_sys::create_index_paths(root.as_raw(), rel.as_raw()) }
}

pub fn clause_selectivity<'a>(root: Ptr<'a, pg_sys::PlannerInfo>, clause: Node<'a>, varno: u32) -> f64 {
    // SAFETY: estimates a live clause of this planning
    unsafe {
        pg_sys::clause_selectivity(root.as_raw(), clause.as_raw(), varno_int(varno), pg_sys::JoinType::JOIN_INNER, std::ptr::null_mut())
    }
}

pub fn clauses_selectivity<'a>(root: Ptr<'a, pg_sys::PlannerInfo>, clauses: List<'a, pg_sys::RestrictInfo>, varno: u32) -> f64 {
    // SAFETY: estimates live clauses of this planning
    unsafe {
        pg_sys::clauselist_selectivity(
            root.as_raw(),
            clauses.as_raw(),
            varno_int(varno),
            pg_sys::JoinType::JOIN_INNER,
            std::ptr::null_mut(),
        )
    }
}

fn varno_int(varno: u32) -> c_int {
    c_int::try_from(varno).unwrap_or(0)
}

/// Whether ordering `keys` is a prefix of ordering `of`.
pub fn pathkeys_contained_in(keys: List<'_, pg_sys::Node>, of: List<'_, pg_sys::Node>) -> bool {
    // SAFETY: compares two live pathkey lists
    unsafe { pg_sys::pathkeys_contained_in(keys.as_raw(), of.as_raw()) }
}

fn made<'a, T>(raw: *mut T) -> Ptr<'a, T> {
    // SAFETY: Postgres's path constructors return a live path of this planning, never null
    unsafe { Ptr::from_raw(raw) }.unwrap_or_else(|| unreachable!("path constructors raise rather than return null"))
}

/// A statistics row: its ctid (block high, block low, offset) and xmin.
type StatsRow = (u16, u16, u16, u32);
/// A column's most common values as text, with the share of rows each has.
type CommonValues = Rc<HashMap<String, f64>>;

thread_local! {
    /// Most common values as text, per statistics row (its ctid and xmin: ANALYZE writes a new
    /// row, so a cached entry is never stale): converting them costs more than the rest of
    /// pg_auth's planning, and the planner asks for them on every statement.
    static COMMON_VALUES: RefCell<HashMap<StatsRow, CommonValues>> = RefCell::new(HashMap::new());
}

/// The column's number of distinct values and its most common values (as text, as ids are
/// listed) with the share of rows each has, from its statistics: None without them. A key
/// expression has statistics when an index is on it.
pub fn column_stats<'a>(root: Ptr<'a, pg_sys::PlannerInfo>, rti: u32, key: Node<'a>) -> Option<(f64, Rc<HashMap<String, f64>>)> {
    let mut vardata = pg_sys::VariableStatData::default();
    let mut isdefault = false;
    // SAFETY: examine_variable fills vardata for a live expression of this planning; its stats
    // tuple is read (most_common) and released with the function it names
    let (distinct, common) = unsafe {
        pg_sys::examine_variable(root.as_raw(), key.as_raw(), varno_int(rti), &raw mut vardata);
        let distinct = pg_sys::get_variable_numdistinct(&raw mut vardata, &raw mut isdefault);
        let common = most_common(&vardata);
        if let (false, Some(free)) = (vardata.statsTuple.is_null(), vardata.freefunc) {
            free(vardata.statsTuple);
        }
        (distinct, common)
    };
    (!isdefault && distinct > 0.0).then_some((distinct, common))
}

/// The most common values of examined statistics, cached per statistics row.
///
/// # Safety
/// `vardata` was filled by examine_variable and its stats tuple not yet released.
unsafe fn most_common(vardata: &pg_sys::VariableStatData) -> Rc<HashMap<String, f64>> {
    let tuple = vardata.statsTuple;
    if tuple.is_null() {
        return Rc::default();
    }
    // SAFETY: a live stats tuple (the caller's contract)
    let (at, xmin) = unsafe { ((*tuple).t_self, (*(*tuple).t_data).t_choice.t_heap.t_xmin.into_inner()) };
    let id = (at.ip_blkid.bi_hi, at.ip_blkid.bi_lo, at.ip_posid, xmin);
    // only a pg_statistic row has a place and an xmin: an expression's statistics from extended
    // statistics are built in memory, and would share one key
    let cached = at.ip_posid != 0;
    if let Some(common) = COMMON_VALUES.with(|c| c.borrow().get(&id).cloned()).filter(|_| cached) {
        return common;
    }
    let mut common = HashMap::new();
    let mut slot = pg_sys::AttStatsSlot::default();
    let flags = (pg_sys::ATTSTATSSLOT_VALUES | pg_sys::ATTSTATSSLOT_NUMBERS).cast_signed();
    // SAFETY: get_attstatsslot fills the slot from the live tuple: nvalues values and nnumbers
    // numbers, read within both counts, freed after
    unsafe {
        if pg_sys::get_attstatsslot(&raw mut slot, tuple, pg_sys::STATISTIC_KIND_MCV.cast_signed(), pg_sys::InvalidOid, flags) {
            let n = usize::try_from(slot.nvalues.min(slot.nnumbers)).unwrap_or(0);
            for i in 0..n {
                common.insert(catalog::output(slot.valuetype, *slot.values.add(i)), f64::from(*slot.numbers.add(i)));
            }
            pg_sys::free_attstatsslot(&raw mut slot);
        }
    }
    let common = Rc::new(common);
    if cached {
        COMMON_VALUES.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() >= 1000 {
                c.clear();
            }
            c.insert(id, common.clone());
        });
    }
    common
}

/// A target list with every column of the relation in order, so rows have the relation's row
/// type, reading only `used` ones (None: all); the others are typed NULLs, so a `count(*)` does
/// not unpack every column. (build_physical_tlist gives up on dropped columns and on columns
/// added with a default; dropped ones are NULL here, added ones read their default.)
pub fn every_column<'a>(
    root: Ptr<'a, pg_sys::PlannerInfo>,
    rel: Ptr<'a, pg_sys::RelOptInfo>,
    used: Option<&[i16]>,
) -> List<'a, pg_sys::TargetEntry> {
    let varno = rel.relid();
    let Some(rte) = root.rte(varno) else { return List::empty() };
    let mut tlist = std::ptr::null_mut();
    // SAFETY: the relation is open (planning holds its lock) while its descriptor is read; the
    // entries are built in the current context
    unsafe {
        let relation = pg_sys::table_open(rte.relid(), pg_sys::NoLock.cast_signed());
        let desc = (*relation).rd_att;
        for attno in 1..=(*desc).natts {
            #[cfg(feature = "pg18")]
            let att = pg_sys::TupleDescAttr(desc, attno - 1);
            // before 18 the attributes are an array in the descriptor itself
            #[cfg(not(feature = "pg18"))]
            let att = (*desc).attrs.as_mut_ptr().add(usize::try_from(attno - 1).unwrap_or(0));
            let number = i16::try_from(attno).unwrap_or(i16::MAX);
            let expr: *mut pg_sys::Expr = if (*att).attisdropped {
                pg_sys::makeNullConst(pg_sys::INT4OID, -1, pg_sys::InvalidOid).cast()
            } else if used.is_some_and(|u| !u.contains(&number)) {
                pg_sys::makeNullConst((*att).atttypid, (*att).atttypmod, (*att).attcollation).cast()
            } else {
                pg_sys::makeVar(varno_int(varno), number, (*att).atttypid, (*att).atttypmod, (*att).attcollation, 0).cast()
            };
            tlist = pg_sys::lappend(tlist, pg_sys::makeTargetEntry(expr, number, std::ptr::null_mut(), false).cast());
        }
        pg_sys::table_close(relation, pg_sys::NoLock.cast_signed());
        List::from_raw(tlist)
    }
}

/// A Result node producing `targets` from the rows of `child`, with its costs.
pub fn result_over<'a>(child: Ptr<'a, pg_sys::Plan>, targets: List<'a, pg_sys::TargetEntry>) -> Ptr<'a, pg_sys::Plan> {
    // SAFETY: a zeroed Result node in the current context, its Plan fields set from a live child
    unsafe {
        let result = pg_sys::palloc0(std::mem::size_of::<pg_sys::Result>()).cast::<pg_sys::Result>();
        (*result).plan.type_ = NodeTag::T_Result;
        let c = child.as_raw();
        (*result).plan.targetlist = targets.as_raw();
        (*result).plan.lefttree = c;
        (*result).plan.startup_cost = (*c).startup_cost;
        (*result).plan.total_cost = (*c).total_cost;
        (*result).plan.plan_rows = (*c).plan_rows;
        (*result).plan.plan_width = (*c).plan_width;
        made(result.cast::<pg_sys::Plan>())
    }
}

/// Custom scan methods, which Postgres finds by name in plans it reads back.
#[repr(transparent)]
pub struct ScanMethods(pg_sys::CustomScanMethods);

// SAFETY: immutable after construction (a static), only read by Postgres's one thread
unsafe impl Sync for ScanMethods {}

impl ScanMethods {
    pub const fn new(name: &'static CStr, create: unsafe extern "C-unwind" fn(*mut pg_sys::CustomScan) -> *mut pg_sys::Node) -> Self {
        ScanMethods(pg_sys::CustomScanMethods { CustomName: name.as_ptr(), CreateCustomScanState: Some(create) })
    }
}

/// pg_auth's side of the planner hooks.
pub struct PlannerHooks {
    /// after the planner built a relation's paths
    pub rel_pathlist: for<'a> fn(Ptr<'a, pg_sys::PlannerInfo>, Ptr<'a, pg_sys::RelOptInfo>, u32, Ptr<'a, pg_sys::RangeTblEntry>),
    /// a custom path chosen: its plan
    pub plan_custom: for<'a> fn(CustomPlanning<'a>) -> Ptr<'a, pg_sys::Plan>,
    pub scan_methods: &'static ScanMethods,
}

/// What Postgres hands a custom path's PlanCustomPath.
pub struct CustomPlanning<'a> {
    pub root: Ptr<'a, pg_sys::PlannerInfo>,
    pub rel: Ptr<'a, pg_sys::RelOptInfo>,
    pub path: Ptr<'a, pg_sys::CustomPath>,
    /// the target list to produce
    pub tlist: List<'a, pg_sys::TargetEntry>,
    pub clauses: List<'a, pg_sys::RestrictInfo>,
    /// the children's plans
    pub children: List<'a, pg_sys::Plan>,
}

static PLANNER_HOOKS: OnceLock<PlannerHooks> = OnceLock::new();
static PREV_REL_PATHLIST: OnceLock<pg_sys::set_rel_pathlist_hook_type> = OnceLock::new();

/// Install the planner hooks (once, from _PG_init).
pub fn install(hooks: PlannerHooks) {
    let methods = std::ptr::from_ref(&hooks.scan_methods.0);
    if PLANNER_HOOKS.set(hooks).is_err() {
        return;
    }
    // SAFETY: as hooks::install: once, before any query, on the backend's only thread; the scan
    // methods are a static
    unsafe {
        let _ = PREV_REL_PATHLIST.set(pg_sys::set_rel_pathlist_hook);
        pg_sys::set_rel_pathlist_hook = Some(rel_pathlist);
        pg_sys::RegisterCustomScanMethods(methods);
    }
}

fn planner_hooks() -> &'static PlannerHooks {
    PLANNER_HOOKS.get().unwrap_or_else(|| unreachable!("the entry points are installed with their hooks"))
}

#[pg_guard]
unsafe extern "C-unwind" fn rel_pathlist(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
    rti: pg_sys::Index,
    rte: *mut pg_sys::RangeTblEntry,
) {
    // SAFETY: the hook it replaced, with the arguments as given
    unsafe {
        if let Some(prev) = PREV_REL_PATHLIST.get().copied().flatten() {
            prev(root, rel, rti, rte);
        }
    }
    // SAFETY: the hook is given the live planner state, relation and range table entry
    let handles = unsafe { (Ptr::from_raw(root), Ptr::from_raw(rel), Ptr::from_raw(rte)) };
    if let (Some(root), Some(rel), Some(rte)) = handles {
        (planner_hooks().rel_pathlist)(root, rel, rti, rte);
    }
}

static PATH_METHODS: PathMethods = PathMethods(pg_sys::CustomPathMethods {
    CustomName: c"AuthzBatch".as_ptr(),
    PlanCustomPath: Some(plan_custom_path),
    ReparameterizeCustomPathByChild: None,
});

#[repr(transparent)]
struct PathMethods(pg_sys::CustomPathMethods);

// SAFETY: immutable (a static), only read by Postgres's one thread
unsafe impl Sync for PathMethods {}

/// A custom path over `children`, with the first child's estimate and costs (callers adjust
/// them), in the order `pathkeys`. `projection`: whether it may produce any target list.
pub fn custom_path<'a>(
    rel: Ptr<'a, pg_sys::RelOptInfo>,
    children: &[Ptr<'a, pg_sys::Path>],
    pathkeys: List<'a, pg_sys::Node>,
    projection: bool,
    private: List<'a, pg_sys::Node>,
) -> Option<Ptr<'a, pg_sys::CustomPath>> {
    let first = *children.first()?;
    let m = rel.memory();
    let paths = m.list(children.iter().copied());
    // SAFETY: a zeroed CustomPath in the current context, its Path fields set from a live child
    // as the planner sets a scan path's; its methods are a static
    unsafe {
        let cp = pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomPath>()).cast::<pg_sys::CustomPath>();
        let f = first.as_raw();
        let p = &raw mut (*cp).path;
        (*p).type_ = NodeTag::T_CustomPath;
        (*p).pathtype = NodeTag::T_CustomScan;
        (*p).parent = rel.as_raw();
        (*p).pathtarget = (*f).pathtarget;
        (*p).param_info = (*f).param_info;
        (*p).rows = (*f).rows;
        #[cfg(feature = "pg18")]
        {
            (*p).disabled_nodes = (*f).disabled_nodes;
        }
        (*p).startup_cost = (*f).startup_cost;
        (*p).total_cost = (*f).total_cost;
        (*p).pathkeys = pathkeys.as_raw();
        (*cp).flags = if projection { pg_sys::CUSTOMPATH_SUPPORT_PROJECTION } else { 0 };
        (*cp).custom_paths = paths.as_raw();
        (*cp).custom_private = private.as_raw();
        (*cp).methods = &raw const PATH_METHODS.0;
        Ptr::from_raw(cp)
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn plan_custom_path(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    // SAFETY: PlanCustomPath is given live planner state, the chosen path, and lists of target
    // entries, restrictions and the children's plans
    let planning = unsafe {
        match (Ptr::from_raw(root), Ptr::from_raw(rel), Ptr::from_raw(best_path)) {
            (Some(root), Some(rel), Some(path)) => Some(CustomPlanning {
                root,
                rel,
                path,
                tlist: List::from_raw(tlist),
                clauses: List::from_raw(clauses),
                children: List::from_raw(custom_plans),
            }),
            _ => None,
        }
    };
    match planning {
        Some(planning) => (planner_hooks().plan_custom)(planning).as_raw(),
        None => std::ptr::null_mut(),
    }
}

/// The CustomScan plan of a custom path.
pub struct CustomScan<'a> {
    pub tlist: List<'a, pg_sys::TargetEntry>,
    /// evaluated on its rows (the plan's qual)
    pub qual: List<'a, pg_sys::Node>,
    pub scanrelid: u32,
    pub flags: u32,
    pub children: List<'a, pg_sys::Plan>,
    pub private: List<'a, pg_sys::Node>,
    /// expressions Postgres fixes up like the plan's own (a key expression)
    pub exprs: List<'a, pg_sys::Node>,
}

impl<'a> CustomScan<'a> {
    pub fn build(self) -> Ptr<'a, pg_sys::Plan> {
        // SAFETY: a zeroed CustomScan in the current context, its fields set from live lists of
        // this planning; its methods are the static installed with the hooks
        unsafe {
            let cs = pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>();
            (*cs).scan.plan.type_ = NodeTag::T_CustomScan;
            (*cs).scan.plan.targetlist = self.tlist.as_raw();
            (*cs).scan.plan.qual = self.qual.as_raw();
            (*cs).scan.scanrelid = self.scanrelid;
            (*cs).flags = self.flags;
            (*cs).custom_plans = self.children.as_raw();
            (*cs).custom_private = self.private.as_raw();
            (*cs).custom_exprs = self.exprs.as_raw();
            (*cs).methods = &raw const planner_hooks().scan_methods.0;
            made(cs.cast::<pg_sys::Plan>())
        }
    }
}
