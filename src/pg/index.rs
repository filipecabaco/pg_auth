//! The permission index (sql/index.sql) read directly through its indexes: no query, so no
//! parsing, planning or plan cache, no `security definer` function and no search_path. The
//! caller passes the subject it resolved, always the current one; every read checks, in the same
//! snapshot as what it returns, that the set is still that subject's, fresh, and the same fill,
//! so a set gone stale or refilled meanwhile is no answer.
//!
//! Reads bypass the tables' privileges, as their owner would read them: only pg_auth's own code
//! calls these, never with a subject the caller chose. The tables and indexes are pg_auth's own,
//! checked once per backend (`Tables::find`).

use super::{Oid, pg_sys};
use pgrx::{FromDatum, IntoDatum};
use std::cell::Cell;
use std::ffi::CStr;

/// A subject's fresh set: its id, size and when its fill finished (a refill keeps the id; the
/// pair names the contents).
pub struct Fresh {
    pub id: i64,
    pub size: i64,
    pub filled_at: i64,
}

/// The snapshot a read uses: the statement's, or a new one (READ COMMITTED; the transaction's
/// under REPEATABLE READ) once the transaction waited for a set committed meanwhile. A transaction
/// that has written reads with a new one either way, as SPI does, so it sees its own writes.
#[derive(Clone, Copy)]
pub enum Snapshot {
    Statement,
    New,
}

/// The current subject's fresh set for (`object_type`, `permission`), if there is one.
pub fn fresh_set(subject: &str, object_type: &str, permission: &str, snapshot: Snapshot) -> Option<Fresh> {
    let tables = Tables::find()?;
    let snapshot = Registered::take(snapshot);
    let found = tables.fresh(subject, object_type, permission, snapshot.0);
    snapshot.release();
    found
}

/// Every id of the fill `filled_at` of set `set_id`, if it is still the subject's fresh set for
/// (`object_type`, `permission`).
pub fn set_ids(set_id: i64, filled_at: i64, subject: &str, object_type: &str, permission: &str, snapshot: Snapshot) -> Option<Vec<String>> {
    let tables = Tables::find()?;
    let snapshot = Registered::take(snapshot);
    let ids = tables
        .same_fill(set_id, filled_at, subject, object_type, permission, snapshot.0)
        .then(|| tables.permitted(set_id, None, snapshot.0));
    snapshot.release();
    ids
}

/// Which of `ids` the fill `filled_at` of set `set_id` holds, if it is still the subject's fresh
/// set: one probe of the primary key per id.
pub fn set_holds(
    set_id: i64,
    filled_at: i64,
    subject: &str,
    object_type: &str,
    permission: &str,
    ids: &[String],
    snapshot: Snapshot,
) -> Option<Vec<String>> {
    let tables = Tables::find()?;
    let snapshot = Registered::take(snapshot);
    let found = tables
        .same_fill(set_id, filled_at, subject, object_type, permission, snapshot.0)
        .then(|| tables.permitted(set_id, Some(ids), snapshot.0));
    snapshot.release();
    found
}

const SCHEMA: &CStr = c"authz_internal";
const EXTENSION: &CStr = c"pg_auth";

// permission_sets
const SET_ID: i32 = 1;
const STATUS: i32 = 5;
const FILLED_AT: i32 = 7;
const SIZE: i32 = 8;
// permitted
const OBJECT_ID: i32 = 2;

/// The tables and the indexes read, found by name in pg_auth's schema (sql/index.sql: a unique
/// constraint and a primary key, named as Postgres names them).
#[derive(Clone, Copy, PartialEq)]
struct Tables {
    sets: Oid,
    by_subject: Oid,
    permitted: Oid,
    permitted_key: Oid,
}

thread_local! {
    /// the tables last found and checked: a check reads pg_depend, so it is made once per backend,
    /// and again when the names lead elsewhere (the extension created again)
    static CHECKED: Cell<Option<Tables>> = const { Cell::new(None) };
}

impl Tables {
    fn find() -> Option<Tables> {
        // SAFETY: syscache lookups by name; missing_ok, so no error when pg_auth's schema is not
        // there
        let tables = unsafe {
            let ns = pg_sys::get_namespace_oid(SCHEMA.as_ptr(), true);
            if ns == pg_sys::InvalidOid {
                return None;
            }
            let rel = |name: &CStr| Some(pg_sys::get_relname_relid(name.as_ptr(), ns)).filter(|o| *o != pg_sys::InvalidOid);
            Tables {
                sets: rel(c"permission_sets")?,
                by_subject: rel(c"permission_sets_subject_object_type_permission_key")?,
                permitted: rel(c"permitted")?,
                permitted_key: rel(c"permitted_pkey")?,
            }
        };
        if CHECKED.get() == Some(tables) {
            return Some(tables);
        }
        tables.check().then(|| {
            CHECKED.set(Some(tables));
            tables
        })
    }

    /// pg_auth's own: its schema belongs to the extension, the columns read have their types, and
    /// each index is valid, unique, whole (not partial) and on its table's key columns in order.
    fn check(self) -> bool {
        let (text, int8, timestamptz) = (pg_sys::TEXTOID, pg_sys::INT8OID, pg_sys::TIMESTAMPTZOID);
        // SAFETY: catalog lookups of relations found by name above, and of their schema; the
        // indexes are opened (and their descriptors read) under a lock, then closed
        unsafe {
            let ext = pg_sys::get_extension_oid(EXTENSION.as_ptr(), true);
            let ns = pg_sys::get_rel_namespace(self.sets);
            if ext == pg_sys::InvalidOid || pg_sys::getExtensionOfObject(pg_sys::NamespaceRelationId, ns) != ext {
                return false;
            }
            let typed = |rel: Oid, columns: &[(i16, Oid)]| columns.iter().all(|(attno, typ)| pg_sys::get_atttype(rel, *attno) == *typ);
            let sets = [(1, int8), (2, text), (3, text), (4, text), (5, text), (7, timestamptz), (8, int8)];
            let keyed = |index: Oid, table: Oid, columns: &[i16]| {
                let rel = pg_sys::index_open(index, pg_sys::AccessShareLock.cast_signed());
                let form = (*rel).rd_index;
                let key = (*form).indkey.values.as_slice(columns.len());
                let ok = (*form).indrelid == table
                    && (*form).indisunique
                    && (*form).indisvalid
                    && i32::from((*form).indnkeyatts) == i32::try_from(columns.len()).unwrap_or(-1)
                    && key == columns
                    && pg_sys::RelationGetIndexPredicate(rel).is_null();
                pg_sys::index_close(rel, pg_sys::AccessShareLock.cast_signed());
                ok
            };
            typed(self.sets, &sets)
                && typed(self.permitted, &[(1, int8), (2, text)])
                && keyed(self.by_subject, self.sets, &[2, 3, 4])
                && keyed(self.permitted_key, self.permitted, &[1, 2])
        }
    }

    /// The subject's set for the pair, when it is fresh.
    fn fresh(&self, subject: &str, object_type: &str, permission: &str, snapshot: pg_sys::Snapshot) -> Option<Fresh> {
        let mut scan = Scan::open(self.sets, self.by_subject, snapshot, 3);
        let mut keys = [
            key(1, pg_sys::F_TEXTEQ, subject.into_datum()),
            key(2, pg_sys::F_TEXTEQ, object_type.into_datum()),
            key(3, pg_sys::F_TEXTEQ, permission.into_datum()),
        ];
        scan.rescan(&mut keys);
        let mut found = None;
        // a unique index: one visible version at most
        if scan.next() && scan.get::<String>(STATUS).as_deref() == Some("fresh") {
            found = Some(Fresh {
                id: scan.get(SET_ID).unwrap_or_default(),
                size: scan.get(SIZE).unwrap_or_default(),
                // timestamptz is an int8 of microseconds
                filled_at: scan.get(FILLED_AT).unwrap_or_default(),
            });
        }
        scan.close();
        free(&keys);
        found
    }

    fn same_fill(
        &self,
        set_id: i64,
        filled_at: i64,
        subject: &str,
        object_type: &str,
        permission: &str,
        snapshot: pg_sys::Snapshot,
    ) -> bool {
        self.fresh(subject, object_type, permission, snapshot).is_some_and(|f| f.id == set_id && f.filled_at == filled_at)
    }

    /// The ids of set `set_id`, or those of `ids` it holds: the index opened once, probed per id.
    fn permitted(&self, set_id: i64, ids: Option<&[String]>, snapshot: pg_sys::Snapshot) -> Vec<String> {
        let mut found = Vec::new();
        let set = set_id.into_datum();
        match ids {
            None => {
                let mut scan = Scan::open(self.permitted, self.permitted_key, snapshot, 1);
                scan.rescan(&mut [key(1, pg_sys::F_INT8EQ, set)]);
                while scan.next() {
                    found.extend(scan.get::<String>(OBJECT_ID));
                }
                scan.close();
            }
            Some(ids) => {
                let mut scan = Scan::open(self.permitted, self.permitted_key, snapshot, 2);
                for id in ids {
                    let mut keys = [key(1, pg_sys::F_INT8EQ, set), key(2, pg_sys::F_TEXTEQ, id.as_str().into_datum())];
                    scan.rescan(&mut keys);
                    if scan.next() {
                        found.push(id.clone());
                    }
                    free(&keys);
                }
                scan.close();
            }
        }
        found
    }
}

/// An equality key on index column `column`. The text columns are `collate "C"`, ScanKeyInit's
/// collation.
fn key(column: i16, procedure: u32, value: Option<pg_sys::Datum>) -> pg_sys::ScanKeyData {
    let mut key = pg_sys::ScanKeyData::default();
    // SAFETY: initializes the key in place; its value lives until `free`, after the scan used it
    unsafe {
        pg_sys::ScanKeyInit(
            &raw mut key,
            column,
            u16::try_from(pg_sys::BTEqualStrategyNumber).unwrap_or_default(),
            Oid::from(procedure),
            value.unwrap_or(pg_sys::Datum::from(0)),
        );
    }
    key
}

/// Free the text values of `keys` (palloc'd by `into_datum`), once no scan reads them.
fn free(keys: &[pg_sys::ScanKeyData]) {
    for key in keys.iter().filter(|k| k.sk_func.fn_oid == Oid::from(pg_sys::F_TEXTEQ)) {
        // SAFETY: a text value made for this key alone, no longer read
        unsafe { pg_sys::pfree(key.sk_argument.cast_mut_ptr()) }
    }
}

/// An index scan of a table, open until `close`. Not a catalog scan (systable_beginscan): that
/// one reads the whole table instead when a session sets ignore_system_indexes. On an error the
/// transaction's abort ends the scan and closes the relations, as for any scan.
struct Scan {
    heap: pg_sys::Relation,
    index: pg_sys::Relation,
    desc: pg_sys::IndexScanDesc,
    slot: *mut pg_sys::TupleTableSlot,
    tuple: *mut pg_sys::HeapTupleData,
}

impl Scan {
    fn open(table: Oid, index: Oid, snapshot: pg_sys::Snapshot, nkeys: i32) -> Scan {
        let lock = pg_sys::AccessShareLock.cast_signed();
        // SAFETY: the relations are opened (the table locked for the transaction, as a query
        // would) and the scan begun with a registered snapshot; all are ended by `close`
        unsafe {
            let heap = pg_sys::table_open(table, lock);
            let index = pg_sys::index_open(index, lock);
            #[cfg(any(feature = "pg15", feature = "pg16", feature = "pg17"))]
            let scan = pg_sys::index_beginscan(heap, index, snapshot, nkeys, 0);
            #[cfg(not(any(feature = "pg15", feature = "pg16", feature = "pg17")))]
            let scan = pg_sys::index_beginscan(heap, index, snapshot, std::ptr::null_mut(), nkeys, 0);
            let slot = pg_sys::table_slot_create(heap, std::ptr::null_mut());
            Scan { heap, index, desc: scan, slot, tuple: std::ptr::null_mut() }
        }
    }

    /// Start over with `keys` (on the index's columns).
    fn rescan(&mut self, keys: &mut [pg_sys::ScanKeyData]) {
        let nkeys = i32::try_from(keys.len()).unwrap_or_default();
        // SAFETY: an open scan, given as many keys as it was begun for; it copies them
        unsafe { pg_sys::index_rescan(self.desc, keys.as_mut_ptr(), nkeys, std::ptr::null_mut(), 0) }
    }

    /// The next matching tuple visible to the snapshot: whether there is one.
    fn next(&mut self) -> bool {
        // SAFETY: an open scan and its table's slot; the tuple is the slot's, read before the
        // next call
        unsafe {
            if !pg_sys::index_getnext_slot(self.desc, pg_sys::ScanDirection::ForwardScanDirection, self.slot) {
                self.tuple = std::ptr::null_mut();
                return false;
            }
            let mut free = false;
            self.tuple = pg_sys::ExecFetchSlotHeapTuple(self.slot, false, &raw mut free);
            true
        }
    }

    /// Column `attno` of the current tuple, copied out; its type is checked (`Tables::check`).
    fn get<T: FromDatum>(&self, attno: i32) -> Option<T> {
        if self.tuple.is_null() {
            return None;
        }
        // SAFETY: the current tuple of this scan's table, with its descriptor; from_datum copies
        unsafe {
            let mut null = false;
            let datum = pg_sys::heap_getattr(self.tuple, attno, (*self.heap).rd_att, &mut null);
            T::from_datum(datum, null)
        }
    }

    fn close(self) {
        let lock = pg_sys::AccessShareLock.cast_signed();
        // SAFETY: opened by `open`, closed once; the table's lock is kept to the transaction's
        // end, as a query keeps it
        unsafe {
            pg_sys::ExecDropSingleTupleTableSlot(self.slot);
            pg_sys::index_endscan(self.desc);
            pg_sys::index_close(self.index, lock);
            pg_sys::table_close(self.heap, pg_sys::NoLock.cast_signed());
        }
    }
}

/// A snapshot registered for the reads of one call.
struct Registered(pg_sys::Snapshot);

impl Registered {
    fn take(snapshot: Snapshot) -> Registered {
        // SAFETY: snapshot management as SPI's: the active snapshot (the statement's) when there
        // is one and the transaction has not written, else the transaction's (a new one under
        // READ COMMITTED, which sees the transaction's own writes), registered while read;
        // released by `release`, or by the transaction's abort on an error
        unsafe {
            // the top transaction's: inside a subtransaction (an EXCEPTION block) the writes
            // before it count too
            let read_only = pg_sys::GetTopTransactionIdIfAny() == pg_sys::InvalidTransactionId;
            let snapshot = match snapshot {
                Snapshot::Statement if read_only && pg_sys::ActiveSnapshotSet() => pg_sys::GetActiveSnapshot(),
                _ => pg_sys::GetTransactionSnapshot(),
            };
            Registered(pg_sys::RegisterSnapshot(snapshot))
        }
    }

    fn release(self) {
        // SAFETY: registered by `take`, once
        unsafe { pg_sys::UnregisterSnapshot(self.0) }
    }
}
