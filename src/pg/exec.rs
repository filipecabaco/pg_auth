//! AuthzBatch's executor: the custom scan's node, its callbacks, the buffered batches it checks,
//! and the per-row loops that run without pgrx's guard. pg_auth's decisions (which child runs,
//! the engine's answers) are rewrite/batch.rs's.
//!
//! Each callback is given the scan's node: `Scan::enter` turns it into a handle and borrows the
//! scan's Rust state (`Batch`, its own allocation) once for the call, which the safe functions
//! below take. Rows are handled through `Slot` and children through `Child`, small handles on
//! what the executor passes around, valid while the scan runs.

use super::pg_sys;
use crate::rewrite::batch::{Adaptive, Check, FIRST_BATCH, IdKind, MAX_BATCH, Mode, int_id};
use crate::sidecar::Loaded;
use pgrx::pg_guard;
use pgrx::pg_sys::NodeTag;
use std::ffi::{CStr, CString, c_int, c_void};
use std::ptr::NonNull;
use std::rc::Rc;

/// AuthzBatch's scan methods, which plans name (installed with the planner hooks).
pub static SCAN_METHODS: super::plan::ScanMethods = super::plan::ScanMethods::new(c"AuthzBatch", create_batch_state);

static EXEC_METHODS: ExecMethods = ExecMethods(pg_sys::CustomExecMethods {
    CustomName: c"AuthzBatch".as_ptr(),
    BeginCustomScan: Some(begin_batch),
    ExecCustomScan: Some(exec_batch),
    EndCustomScan: Some(end_batch),
    ReScanCustomScan: Some(rescan_batch),
    MarkPosCustomScan: None,
    RestrPosCustomScan: None,
    EstimateDSMCustomScan: None,
    InitializeDSMCustomScan: None,
    ReInitializeDSMCustomScan: None,
    InitializeWorkerCustomScan: None,
    ShutdownCustomScan: None,
    ExplainCustomScan: Some(explain_batch),
});

#[repr(transparent)]
struct ExecMethods(pg_sys::CustomExecMethods);

// SAFETY: immutable (a static), only read by Postgres's one thread
unsafe impl Sync for ExecMethods {}

/// The scan's node; `batch` lives in the query's memory context and is dropped with it.
#[repr(C)]
struct BatchState {
    css: pg_sys::CustomScanState,
    batch: *mut Batch,
    /// the listed-ids child, when there is one (see pass_rows)
    listed: *mut pg_sys::PlanState,
    /// the child whose rows checked_rows checks
    checked: *mut pg_sys::PlanState,
}

struct Batch {
    check: Check,
    attno: i16,
    typoutput: pg_sys::Oid,
    /// a key expression (null for a column, `attno`) and the context it is evaluated in
    key: *mut pg_sys::ExprState,
    key_context: *mut pg_sys::ExprContext,
    /// how the id column is matched against an index set (Mode::Indexed)
    ids: IdKind,
    /// holds the buffered tuples; reset for every batch
    memory: pg_sys::MemoryContext,
    tuples: Vec<pg_sys::HeapTuple>,
    allowed: Vec<bool>,
    next: usize,
    size: usize,
    done: bool,
}

/// A handle on an AuthzBatch node during one of its callbacks.
#[derive(Clone, Copy)]
struct Scan(NonNull<BatchState>);

/// A handle on one of the scan's children.
#[derive(Clone, Copy)]
struct Child(NonNull<pg_sys::PlanState>);

/// A handle on a row in a slot, valid until its producer is asked for the next one.
#[derive(Clone, Copy)]
struct Slot(NonNull<pg_sys::TupleTableSlot>);

impl Scan {
    /// The scan and its state, for one callback.
    ///
    /// # Safety
    /// `node` is an AuthzBatch node (create_batch_state), live for the call, begun (its batch
    /// set by begin_batch); no other borrow of its state is alive (callbacks do not nest for one
    /// node: a scan's children are other nodes).
    unsafe fn enter<'s>(node: *mut pg_sys::CustomScanState) -> Option<(Scan, &'s mut Batch)> {
        let scan = Scan(NonNull::new(node.cast::<BatchState>())?);
        // SAFETY: the caller's contract: the state is begun and borrowed only here
        let batch = unsafe { (*scan.0.as_ptr()).batch.as_mut()? };
        Some((scan, batch))
    }

    fn raw(self) -> *mut BatchState {
        self.0.as_ptr()
    }

    /// The `i`th child.
    fn child(self, i: usize) -> Option<Child> {
        // SAFETY: a live scan node; custom_ps is the list of its children's states (begin_batch)
        unsafe {
            let children = super::List::<pg_sys::PlanState>::from_raw((*self.raw()).css.custom_ps);
            children.get(i).map(|c| Child(NonNull::new_unchecked(c.as_raw())))
        }
    }

    fn children(self) -> impl Iterator<Item = Child> {
        (0..).map_while(move |i| self.child(i))
    }

    /// Have Postgres call `rows` for this node's next rows instead of ExecCustomScan
    /// (EXPLAIN ANALYZE's ExecProcNodeInstr calls ExecProcNodeReal; otherwise Postgres calls
    /// ExecProcNode). `checked`: the child whose rows `rows` reads.
    fn run_directly(
        self,
        rows: unsafe extern "C-unwind" fn(*mut pg_sys::PlanState) -> *mut pg_sys::TupleTableSlot,
        checked: Option<Child>,
    ) {
        // SAFETY: a live scan node, whose executor entry points are its own to set
        unsafe {
            let state = self.raw();
            if let Some(child) = checked {
                (*state).checked = child.0.as_ptr();
            }
            let ps = &raw mut (*state).css.ss.ps;
            (*ps).ExecProcNodeReal = Some(rows);
            if (*ps).instrument.is_null() {
                (*ps).ExecProcNode = Some(rows);
            }
        }
    }

    /// Run one of the guard-free row functions now, for the row this call returns.
    fn rows_now(
        self,
        rows: unsafe extern "C-unwind" fn(*mut pg_sys::PlanState) -> *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: a live scan node, which is the plan state those functions expect
        unsafe { rows(&raw mut (*self.raw()).css.ss.ps) }
    }

    fn scan_slot(self) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: a live, begun scan node has its scan slot
        unsafe { (*self.raw()).css.ss.ss_ScanTupleSlot }
    }

    /// Empty the scan slot (it may point into the buffer about to be freed).
    fn clear_scan_slot(self) {
        // SAFETY: the scan's own slot
        unsafe { pg_sys::ExecClearTuple(self.scan_slot()) };
    }

    /// `tuple` (a buffered row) in the scan slot, as the scan's next row.
    fn store(self, tuple: pg_sys::HeapTuple) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: a heap tuple of the scan's buffer, which lives until the next fill or rescan,
        // after this row is consumed; the scan slot holds heap tuples (create_batch_state)
        unsafe {
            #[cfg(not(feature = "pg15"))]
            let slot = pg_sys::ExecStoreHeapTuple(tuple, self.scan_slot(), false);
            // 15's custom scans have a virtual scan slot (no slotOps to ask for heap tuples)
            #[cfg(feature = "pg15")]
            let slot = {
                pg_sys::ExecForceStoreHeapTuple(tuple, self.scan_slot(), false);
                self.scan_slot()
            };
            slot
        }
    }

    /// A copy of `row` in the scan slot, as the scan's next row.
    fn copy(self, row: Slot) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: copies a child's live row into the scan's own slot
        unsafe { pg_sys::ExecCopySlot(self.scan_slot(), row.0.as_ptr()) }
    }

    /// The parameters that changed since this scan last ran.
    fn changed_params(self) -> *mut pg_sys::Bitmapset {
        // SAFETY: a live scan node
        unsafe { (*self.raw()).css.ss.ps.chgParam }
    }

    /// The column the scan checks, as EXPLAIN names it.
    fn column_name(self, attno: i16) -> String {
        if attno <= 0 {
            return "its key expression".into();
        }
        // SAFETY: a begun scan has its relation open
        let relid = unsafe { (*(*self.raw()).css.ss.ss_currentRelation).rd_id };
        super::catalog::attribute_name(relid, attno).unwrap_or_else(|| "?".into())
    }
}

impl Child {
    /// Its next row (ExecProcNode, guarded: an error becomes a panic pgrx turns back into it);
    /// None at the end.
    fn next(self) -> Option<Slot> {
        // SAFETY: a live child of a running scan
        let slot = unsafe { pg_sys::ExecProcNode(self.0.as_ptr()) };
        Slot::row(slot)
    }

    /// Pass the scan's changed parameters to the child, so its next row rescans it; rescan it
    /// now when none changed.
    fn rescan(self, changed: *mut pg_sys::Bitmapset) {
        // SAFETY: a live child; `changed` is its parent's parameter set (or null)
        unsafe {
            let child = self.0.as_ptr();
            if !changed.is_null() {
                pg_sys::UpdateChangedParamSet(child, changed);
            }
            if (*child).chgParam.is_null() {
                pg_sys::ExecReScan(child);
            }
        }
    }

    fn end(self) {
        // SAFETY: a live child, ended once with its parent
        unsafe { pg_sys::ExecEndNode(self.0.as_ptr()) }
    }
}

impl Slot {
    /// The slot as a row: None for null or an empty slot (no more rows).
    fn row(slot: *mut pg_sys::TupleTableSlot) -> Option<Slot> {
        // SAFETY: the executor returns null or a live slot
        (!is_empty(slot)).then(|| Slot(unsafe { NonNull::new_unchecked(slot) }))
    }
}

/// Whether the executor returned no row: null, or an empty slot.
fn is_empty(slot: *mut pg_sys::TupleTableSlot) -> bool {
    // SAFETY: null, or a live slot whose flags are read
    slot.is_null() || u32::from(unsafe { (*slot).tts_flags }) & pg_sys::TTS_FLAG_EMPTY != 0
}

impl Batch {
    /// The row's key: its column, or its key expression evaluated on the row (None: NULL).
    fn key(&self, row: Slot) -> Option<pg_sys::Datum> {
        let mut isnull = false;
        // SAFETY: a live row; the key expression and its context were initialised for this scan
        // (begin_batch), and its per-tuple memory is reset before each row
        let datum = unsafe {
            if self.key.is_null() {
                pg_sys::slot_getattr(row.0.as_ptr(), c_int::from(self.attno), &raw mut isnull)
            } else {
                pg_sys::MemoryContextReset((*self.key_context).ecxt_per_tuple_memory);
                (*self.key_context).ecxt_scantuple = row.0.as_ptr();
                pg_sys::ExecEvalExprSwitchContext(self.key, self.key_context, &raw mut isnull)
            }
        };
        (!isnull).then_some(datum)
    }

    /// The row's key as the policy sees it (`key::text`), or None when NULL (denied).
    fn id_text(&self, row: Slot) -> Option<String> {
        let datum = self.key(row)?;
        // SAFETY: the key type's output function, on a value of that type; the string is copied
        // and freed
        unsafe {
            let out = pg_sys::OidOutputFunctionCall(self.typoutput, datum);
            let id = CStr::from_ptr(out).to_string_lossy().into_owned();
            pg_sys::pfree(out.cast::<c_void>());
            Some(id)
        }
    }

    /// Whether the loaded set allows the row's id (NULL: denied).
    fn allows(&self, row: Slot, set: &Loaded) -> bool {
        let Some(datum) = self.key(row) else { return false };
        match int_id(self.ids, datum.value() as u64) {
            Some(id) => set.ints.contains(&id),
            None => self.id_text(row).is_some_and(|id| set.ids.contains(&id)),
        }
    }

    /// A copy of the row in the batch's memory (which outlives the child's next row).
    fn buffer(&mut self, row: Slot) {
        // SAFETY: copies a live row into the batch's own memory context, then restores the
        // current one
        unsafe {
            let previous = pg_sys::CurrentMemoryContext;
            pg_sys::CurrentMemoryContext = self.memory;
            self.tuples.push(pg_sys::ExecCopySlotHeapTuple(row.0.as_ptr()));
            pg_sys::CurrentMemoryContext = previous;
        }
    }

    /// Drop the buffered rows.
    fn reset_buffer(&mut self) {
        // SAFETY: the batch's own context, whose tuples are forgotten with it
        unsafe { pg_sys::MemoryContextReset(self.memory) };
        self.tuples.clear();
        self.next = 0;
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn create_batch_state(_cscan: *mut pg_sys::CustomScan) -> *mut pg_sys::Node {
    // SAFETY: a zeroed node in the executor's context; its methods are a static, and buffered
    // heap tuples are stored into the scan slot as they are
    unsafe {
        let state = pg_sys::palloc0(std::mem::size_of::<BatchState>()).cast::<BatchState>();
        (*state).css.ss.ps.type_ = NodeTag::T_CustomScanState;
        (*state).css.methods = &raw const EXEC_METHODS.0;
        #[cfg(not(feature = "pg15"))]
        {
            (*state).css.slotOps = &raw const pg_sys::TTSOpsHeapTuple;
        }
        state.cast::<pg_sys::Node>()
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn begin_batch(node: *mut pg_sys::CustomScanState, estate: *mut pg_sys::EState, eflags: i32) {
    // SAFETY: BeginCustomScan is given a node of ours (create_batch_state) and its executor
    // state; the CustomScan it runs is the one plan_batch built, whose private list holds
    // [type, permission, column, single, key, adaptive stats, common values, constant]
    unsafe {
        let cs = (*node).ss.ps.plan.cast::<pg_sys::CustomScan>();
        let mut children = std::ptr::null_mut();
        for plan in super::List::<pg_sys::Plan>::from_raw((*cs).custom_plans).iter() {
            children = pg_sys::lappend(children, pg_sys::ExecInitNode(plan.as_raw(), estate, eflags).cast());
        }
        let children = super::List::<pg_sys::PlanState>::from_raw(children);
        let listed = children.len() == 2;
        if let (true, Some(first)) = (listed, children.get(0)) {
            (*node.cast::<BatchState>()).listed = first.as_raw();
        }
        (*node).custom_ps = children.as_raw();

        // checked_rows evaluates AuthzBatch's condition and projection on the child's row, whose
        // slot type is not the scan slot's: compile them again (ExecInitCustomScan did) for any
        // type
        (*node).ss.ps.scanopsfixed = false;
        let varno = if (*cs).custom_scan_tlist.is_null() { c_int::try_from((*cs).scan.scanrelid).unwrap_or(0) } else { pg_sys::INDEX_VAR };
        pg_sys::ExecAssignScanProjectionInfoWithVarno(&raw mut (*node).ss, varno);
        (*node).ss.ps.qual = pg_sys::ExecInitQual((*cs).scan.plan.qual, node.cast());
        // and pass_rows and checked_rows may return rows in the child's own slot: tell parents
        // not to specialise for one slot type
        (*node).ss.ps.resultopsfixed = false;

        let private = super::List::<pg_sys::Node>::from_raw((*cs).custom_private);
        let string = |i: usize| {
            private
                .get(i)
                .and_then(super::Node::cast::<pg_sys::String>)
                .map(|s| CStr::from_ptr((*s.as_raw()).sval).to_string_lossy().into_owned())
        };
        let integer = |i: usize| private.get(i).and_then(super::Node::cast::<pg_sys::Integer>).map_or(0, |n| (*n.as_raw()).ival);
        let attno = i16::try_from(integer(2)).unwrap_or(0);
        let adaptive = string(5).and_then(|stats| Adaptive::parse(&stats, &string(6).unwrap_or_default()));
        let (key, key_context, typid) = match super::List::<pg_sys::Expr>::from_raw((*cs).custom_exprs).get(0) {
            Some(expr) if attno <= 0 => (
                pg_sys::ExecInitExpr(expr.as_raw(), node.cast()),
                pg_sys::CreateExprContext(estate),
                pg_sys::exprType(expr.as_raw().cast()),
            ),
            _ => (std::ptr::null_mut(), std::ptr::null_mut(), pg_sys::get_atttype((*(*node).ss.ss_currentRelation).rd_id, attno)),
        };
        let (mut typoutput, mut varlena) = (pg_sys::InvalidOid, false);
        pg_sys::getTypeOutputInfo(typid, &raw mut typoutput, &raw mut varlena);
        let ids = match typid {
            pg_sys::INT2OID => IdKind::Int2,
            pg_sys::INT4OID => IdKind::Int4,
            pg_sys::INT8OID if pg_sys::get_typbyval(typid) => IdKind::Int8,
            _ => IdKind::Text,
        };
        let batch = Batch {
            check: Check {
                object_type: string(0).unwrap_or_default(),
                permission: string(1).unwrap_or_default(),
                listed,
                single: integer(3) != 0,
                adaptive,
                pinned: string(7).filter(|s| !s.is_empty()),
                mode: None,
                set: None,
                lookups: 0,
            },
            attno,
            typoutput,
            key,
            key_context,
            ids,
            memory: pg_sys::AllocSetContextCreateInternal(
                pg_sys::CurrentMemoryContext,
                c"pg_auth batch".as_ptr(),
                pg_sys::ALLOCSET_DEFAULT_MINSIZE as usize,
                pg_sys::ALLOCSET_DEFAULT_INITSIZE as usize,
                pg_sys::ALLOCSET_DEFAULT_MAXSIZE as usize,
            ),
            tuples: Vec::new(),
            allowed: Vec::new(),
            next: 0,
            size: FIRST_BATCH,
            done: false,
        };
        (*node.cast::<BatchState>()).batch = pgrx::PgMemoryContexts::CurrentMemoryContext.leak_and_drop_on_delete(batch);
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn exec_batch(node: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: ExecCustomScan is given a begun node of ours
    let Some((scan, b)) = (unsafe { Scan::enter(node) }) else { return std::ptr::null_mut() };
    // a listed subject: the listed child's rows are AuthzBatch's output as they are, so its
    // later rows skip ExecCustomScan and this function
    match b.check.mode() {
        Mode::Pass => {
            scan.run_directly(pass_rows, None);
            scan.rows_now(pass_rows)
        }
        Mode::Unchecked(i) => {
            scan.run_directly(unchecked_rows, scan.child(i));
            scan.rows_now(unchecked_rows)
        }
        // the set was loaded before the scan started (nothing buffered)
        Mode::Indexed(i) if b.tuples.is_empty() && b.key.is_null() => {
            scan.run_directly(checked_rows, scan.child(i));
            scan.rows_now(checked_rows)
        }
        // SAFETY: ExecScan over this node, with its own access functions
        _ => unsafe { pg_sys::ExecScan(&raw mut (*node).ss, Some(next_allowed), Some(recheck)) },
    }
}

/// The next row the subject may see.
#[pg_guard]
unsafe extern "C-unwind" fn next_allowed(ss: *mut pg_sys::ScanState) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: ExecScan calls this with our node (its ScanState is the node's first field)
    let Some((scan, b)) = (unsafe { Scan::enter(ss.cast()) }) else { return std::ptr::null_mut() };
    let child = match b.check.mode() {
        Mode::Nothing => return std::ptr::null_mut(),
        // exec_batch returns the listed child's rows itself
        Mode::Pass | Mode::Unchecked(_) => unreachable!("pass-through rows do not go through ExecScan"),
        Mode::Batch(i) | Mode::Indexed(i) => i,
    };
    let Some(child_scan) = scan.child(child) else { return std::ptr::null_mut() };
    loop {
        if b.next < b.tuples.len() {
            let i = b.next;
            b.next += 1;
            if b.allowed[i] {
                return scan.store(b.tuples[i]);
            }
            continue;
        }
        if b.done {
            return std::ptr::null_mut();
        }
        // buffered rows first: the set may have loaded while answering a batch
        if matches!(b.check.mode, Some(Mode::Indexed(_))) {
            return next_indexed(scan, b, child_scan);
        }
        fill(scan, b, child_scan, child);
    }
}

/// The next row of `child` the loaded set allows, copied into the scan slot. Unlike batches,
/// nothing is buffered and only allowed rows are copied.
fn next_indexed(scan: Scan, b: &mut Batch, child: Child) -> *mut pg_sys::TupleTableSlot {
    // the set loaded mid-scan and the buffered rows are drained: later rows go through
    // checked_rows (this one still returns through ExecScan); a key expression stays here
    if b.key.is_null() {
        scan.run_directly(checked_rows, Some(child));
    }
    let Some(set) = b.check.set.as_ref().map(|(_, set)| Rc::clone(set)) else { return std::ptr::null_mut() };
    loop {
        let Some(row) = child.next() else {
            b.done = true;
            b.check.count_lookups();
            return std::ptr::null_mut();
        };
        b.check.lookups += 1;
        if b.allows(row, &set) {
            return scan.copy(row);
        }
    }
}

/// Read the next batch from the child and check its ids with one request.
fn fill(scan: Scan, b: &mut Batch, child: Child, index: usize) {
    scan.clear_scan_slot();
    b.reset_buffer();
    let mut ids = Vec::with_capacity(b.size);
    while b.tuples.len() < b.size {
        // the child runs in the executor's context: index scans allocate their state lazily
        let Some(row) = child.next() else {
            b.done = true;
            break;
        };
        b.buffer(row);
        ids.push(b.id_text(row));
    }
    b.size = (b.size * 2).min(MAX_BATCH);
    b.allowed = b.check.answer(&ids, index);
}

/// Only reached by EvalPlanQual, which plain reads (the only plans with AuthzBatch) never run.
#[pg_guard]
unsafe extern "C-unwind" fn recheck(_ss: *mut pg_sys::ScanState, _slot: *mut pg_sys::TupleTableSlot) -> bool {
    true
}

#[pg_guard]
unsafe extern "C-unwind" fn rescan_batch(node: *mut pg_sys::CustomScanState) {
    // SAFETY: ReScanCustomScan is given a begun node of ours
    let Some((scan, b)) = (unsafe { Scan::enter(node) }) else { return };
    scan.clear_scan_slot();
    b.reset_buffer();
    b.allowed.clear();
    b.size = FIRST_BATCH;
    b.done = false;
    b.check.count_lookups();
    // changed parameters reach the children through chgParam, so their next ExecProcNode
    // rescans them
    for child in scan.children() {
        child.rescan(scan.changed_params());
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn end_batch(node: *mut pg_sys::CustomScanState) {
    // SAFETY: EndCustomScan is given a begun node of ours
    let Some((scan, b)) = (unsafe { Scan::enter(node) }) else { return };
    b.check.count_lookups();
    for child in scan.children() {
        child.end();
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn explain_batch(
    node: *mut pg_sys::CustomScanState,
    _ancestors: *mut pg_sys::List,
    es: *mut pg_sys::ExplainState,
) {
    // SAFETY: ExplainCustomScan is given a begun node of ours
    let Some((scan, b)) = (unsafe { Scan::enter(node) }) else { return };
    let check = CString::new(b.check.explain(&scan.column_name(b.attno))).unwrap_or_default();
    // SAFETY: adds a property to the live explain state; both strings outlive the call
    unsafe { pg_sys::ExplainPropertyText(c"Authz Check".as_ptr(), check.as_ptr(), es) };
}

// ---- rows without pgrx's guard ----------------------------------------------------------------
//
// The functions below are called by Postgres directly as the node's ExecProcNode (run_directly),
// once per row. Per row, the path through ExecCustomScan and pgrx's guard around each call into
// Postgres (a sigsetjmp) cost a broad scan ~40 ns, twice its plain RLS time. So they call only
// the `unguarded` declarations, hold nothing to drop, and cannot panic: an error in Postgres
// longjmps over their frames as over C. Their bodies are one unsafe block each: they are what
// the executor's own C would be.

/// The Postgres functions the row functions call, declared without pgrx's guard. Inline functions
/// are reached through pgrx's C shims, so they behave as each Postgres version defines them.
mod unguarded {
    use pgrx::pg_sys;
    use std::ffi::{c_char, c_int, c_void};
    unsafe extern "C-unwind" {
        #[link_name = "ExecReScan"]
        pub fn exec_rescan(node: *mut pg_sys::PlanState);
        #[link_name = "slot_getattr__pgrx_cshim"]
        pub fn slot_getattr(slot: *mut pg_sys::TupleTableSlot, attnum: c_int, isnull: *mut bool) -> pg_sys::Datum;
        #[link_name = "MemoryContextReset"]
        pub fn memory_context_reset(context: pg_sys::MemoryContext);
        #[link_name = "ExecQual__pgrx_cshim"]
        pub fn exec_qual(state: *mut pg_sys::ExprState, econtext: *mut pg_sys::ExprContext) -> bool;
        #[link_name = "ExecProject__pgrx_cshim"]
        pub fn exec_project(proj: *mut pg_sys::ProjectionInfo) -> *mut pg_sys::TupleTableSlot;
        #[link_name = "OidOutputFunctionCall"]
        pub fn output_function_call(function: pg_sys::Oid, value: pg_sys::Datum) -> *mut c_char;
        #[link_name = "pfree"]
        pub fn pfree(pointer: *mut c_void);
    }
}

/// A child's next row, as Postgres's inline ExecProcNode gets it (rescanning first when its
/// parameters changed: rescan_batch leaves that to here).
///
/// # Safety
/// `child` is a live child of a running scan.
unsafe fn next_row(child: *mut pg_sys::PlanState) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: the caller's contract
    unsafe {
        if !(*child).chgParam.is_null() {
            unguarded::exec_rescan(child);
        }
        match (*child).ExecProcNode {
            Some(exec) => exec(child),
            None => std::ptr::null_mut(),
        }
    }
}

/// AuthzBatch's own condition and projection on a child's row: the row as the scan's output, or
/// None when the condition rejects it (counted for EXPLAIN ANALYZE).
///
/// # Safety
/// `ps` is the scan's plan state, `slot` a live row of one of its children.
unsafe fn qualify(ps: *mut pg_sys::PlanState, slot: *mut pg_sys::TupleTableSlot) -> Option<*mut pg_sys::TupleTableSlot> {
    // SAFETY: the caller's contract; begin_batch compiled the condition and projection for any
    // slot type
    unsafe {
        let econtext = (*ps).ps_ExprContext;
        unguarded::memory_context_reset((*econtext).ecxt_per_tuple_memory);
        (*econtext).ecxt_scantuple = slot;
        if !(*ps).qual.is_null() && !unguarded::exec_qual((*ps).qual, econtext) {
            if !(*ps).instrument.is_null() {
                (*(*ps).instrument).nfiltered1 += 1.0;
            }
            return None;
        }
        Some(if (*ps).ps_ProjInfo.is_null() { slot } else { unguarded::exec_project((*ps).ps_ProjInfo) })
    }
}

/// A listed subject's next row, straight from the listed child.
unsafe extern "C-unwind" fn pass_rows(ps: *mut pg_sys::PlanState) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: Postgres calls this as our node's ExecProcNode, with the node (run_directly), whose
    // listed child begin_batch set
    unsafe {
        let slot = next_row((*ps.cast::<BatchState>()).listed);
        if is_empty(slot) { std::ptr::null_mut() } else { slot }
    }
}

/// The next row of the checked child that the loaded set allows, as AuthzBatch's output: what
/// ExecScan over next_indexed returns, without the copy and the per-call guards (~90 ns a row
/// there). AuthzBatch's own condition and projection run on the child's row. Rows checked are
/// counted at rescan and end (count_lookups allocates).
unsafe extern "C-unwind" fn checked_rows(ps: *mut pg_sys::PlanState) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: Postgres calls this as our node's ExecProcNode (run_directly, installed with a
    // loaded set and the checked child); the batch is borrowed for this call only, and the set
    // stays until the scan ends
    unsafe {
        let state = ps.cast::<BatchState>();
        let b = &mut *(*state).batch;
        let child = (*state).checked;
        let Some((_, set)) = &b.check.set else { return std::ptr::null_mut() };
        let set: &Loaded = set;
        loop {
            let slot = next_row(child);
            if is_empty(slot) {
                b.done = true;
                return std::ptr::null_mut();
            }
            b.check.lookups += 1;
            let mut isnull = false;
            let id = unguarded::slot_getattr(slot, c_int::from(b.attno), &raw mut isnull);
            let allowed = !isnull
                && if let Some(id) = int_id(b.ids, id.value() as u64) {
                    set.ints.contains(&id)
                } else {
                    let text = unguarded::output_function_call(b.typoutput, id);
                    let found = CStr::from_ptr(text).to_str().is_ok_and(|text| set.ids.contains(text));
                    unguarded::pfree(text.cast::<c_void>());
                    found
                };
            if allowed && let Some(out) = qualify(ps, slot) {
                return out;
            }
        }
    }
}

/// The next row of `checked` when the one id the query pins every row to is allowed: no check, but
/// AuthzBatch's own condition (the conditions that may leak run only here, after authorization)
/// and projection, as checked_rows runs them. Not the listed child's pass-through: this child
/// reads the columns AuthzBatch needs, not AuthzBatch's output.
unsafe extern "C-unwind" fn unchecked_rows(ps: *mut pg_sys::PlanState) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: as checked_rows (installed with the checked child)
    unsafe {
        let state = ps.cast::<BatchState>();
        let child = (*state).checked;
        loop {
            let slot = next_row(child);
            if is_empty(slot) {
                (*(*state).batch).done = true;
                return std::ptr::null_mut();
            }
            if let Some(out) = qualify(ps, slot) {
                return out;
            }
        }
    }
}
