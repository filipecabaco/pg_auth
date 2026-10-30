//! Safe wrappers around the few catalog lookups the safe modules need. Each takes plain values
//! (OIDs) and returns owned Rust data, so callers never handle Postgres pointers.

use pgrx::pg_sys;
use std::ffi::CStr;

/// Current value of a configuration setting, or None when unset or empty.
pub fn config_option(name: &str) -> Option<String> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: GetConfigOption returns NULL (missing_ok) or a pointer to the setting's current
    // value, which stays valid until the setting changes; it is copied immediately.
    let p = unsafe { pg_sys::GetConfigOption(name.as_ptr(), true, false) };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null, NUL-terminated string from Postgres.
    let value = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    (!value.is_empty()).then_some(value)
}

/// The first column of the first row of a read-only query, or None (no row, NULL, or an error).
///
/// Not `Spi::get_one*`: pgrx runs those as updates, which assigns the transaction an id. On a
/// query path that would turn every read request into a writing transaction (a WAL flush at
/// commit, XIDs consumed) and fail on a read replica, where no id can be assigned.
pub fn read_one<A: pgrx::FromDatum + pgrx::IntoDatum>(query: &str, args: &[pgrx::datum::DatumWithOid]) -> Option<A> {
    execute(query, args, Read::Current)
}

/// How a prepared query reads.
#[derive(Clone, Copy)]
enum Read {
    /// read-only with the statement's snapshot, until the transaction has written: then with a
    /// new one, which sees its own writes (as pgrx's select)
    Current,
    /// read-only, with the latest snapshot whatever the isolation level
    Latest,
}

thread_local! {
    /// pg_auth's own queries, prepared once per backend, by text and argument types: kept plans
    /// (SPI_keepplan) live in the plan cache, which revalidates them when what they read changes
    /// (the extension created again, say). Every query is a fixed text with parameters, so the
    /// cache stays small.
    static PREPARED: std::cell::RefCell<std::collections::HashMap<(String, Vec<pg_sys::Oid>), usize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Prepare `query` (with parameters of `types`) and keep its plan, as its first execution would:
/// for a warm-up before a backend's first request. Whether it is prepared.
pub fn prepare(query: &str, types: &[pg_sys::Oid]) -> bool {
    let Ok(text) = std::ffi::CString::new(query) else { return false };
    let key = (query.to_string(), types.to_vec());
    if PREPARED.with(|p| p.borrow().contains_key(&key)) {
        return true;
    }
    let mut types = types.to_vec();
    let Ok(count) = i32::try_from(types.len()) else { return false };
    pgrx::Spi::connect(|_client| {
        // SAFETY: inside the SPI connection just opened; a kept plan is owned by the plan cache
        // for the life of the backend.
        unsafe {
            let plan = pg_sys::SPI_prepare(text.as_ptr(), count, types.as_mut_ptr());
            if plan.is_null() || pg_sys::SPI_keepplan(plan) != 0 {
                return false;
            }
            PREPARED.with(|p| p.borrow_mut().insert(key, plan as usize));
            true
        }
    })
}

/// Run `query`'s kept plan, preparing it the first time: its first column of its first row.
fn execute<A: pgrx::FromDatum + pgrx::IntoDatum>(query: &str, args: &[pgrx::datum::DatumWithOid], read: Read) -> Option<A> {
    let text = std::ffi::CString::new(query).ok()?;
    let mut types: Vec<pg_sys::Oid> = args.iter().map(pgrx::datum::DatumWithOid::oid).collect();
    let count = i32::try_from(types.len()).ok()?;
    let mut values: Vec<pg_sys::Datum> =
        args.iter().map(|a| a.datum().map_or(pg_sys::Datum::from(0), pgrx::datum::Datum::sans_lifetime)).collect();
    let nulls: Vec<u8> = args.iter().map(|a| if a.datum().is_some() { b' ' } else { b'n' }).collect();
    let key = (query.to_string(), types.clone());
    pgrx::Spi::connect(|_client| {
        // SAFETY: inside the SPI connection just opened. A kept plan is owned by the plan cache
        // for the life of the backend and only executed here; the arrays hold one entry per
        // argument and outlive the call; the snapshot is copied when SPI pushes it; the result is
        // read before the connection closes, and from_datum copies it out of SPI's memory.
        unsafe {
            let plan = if let Some(plan) = PREPARED.with(|p| p.borrow().get(&key).copied()) {
                plan as pg_sys::SPIPlanPtr
            } else {
                let plan = pg_sys::SPI_prepare(text.as_ptr(), count, types.as_mut_ptr());
                if plan.is_null() || pg_sys::SPI_keepplan(plan) != 0 {
                    return None;
                }
                PREPARED.with(|p| p.borrow_mut().insert(key, plan as usize));
                plan
            };
            let rc = match read {
                Read::Current => {
                    let read_only = pg_sys::GetCurrentTransactionIdIfAny() == pg_sys::InvalidTransactionId;
                    pg_sys::SPI_execute_plan(plan, values.as_mut_ptr(), nulls.as_ptr().cast(), read_only, 1)
                }
                Read::Latest => pg_sys::SPI_execute_snapshot(
                    plan,
                    values.as_mut_ptr(),
                    nulls.as_ptr().cast(),
                    pg_sys::GetLatestSnapshot(),
                    std::ptr::null_mut(),
                    true,
                    false,
                    1,
                ),
            };
            first_value(rc)
        }
    })
}

/// Run a statement through SPI as a volatile function does (`read_only = false`), without
/// assigning a transaction id: for utility statements such as EXPLAIN, which SPI refuses to run
/// read-only, and which `Spi::run` would run as an update (see `read_one`).
pub fn run_volatile(query: &str) -> Result<(), String> {
    let query = std::ffi::CString::new(query).map_err(|e| e.to_string())?;
    pgrx::Spi::connect(|_client| {
        // SAFETY: inside the SPI connection just opened; `query` is NUL-terminated and outlives
        // the call. Errors are raised by Postgres and reach the caller as a pgrx error.
        let rc = unsafe { pg_sys::SPI_execute(query.as_ptr(), false, 0) };
        if rc < 0 { Err(format!("SPI_execute failed ({rc})")) } else { Ok(()) }
    })
}

/// The first column of the first row the last SPI query returned, copied out of SPI's memory;
/// None unless the column has the type `A` reads (a query's type names resolve through the
/// caller's search_path: a type of the same name elsewhere is not decoded as this one).
///
/// # Safety
/// Inside the SPI connection that ran the query, with `rc` its result code.
unsafe fn first_value<A: pgrx::FromDatum + pgrx::IntoDatum>(rc: i32) -> Option<A> {
    // SAFETY: the caller's contract: SPI's result globals describe the query just run, whose
    // first row and descriptor are live until the connection closes; from_datum copies the value
    unsafe {
        let table = pg_sys::SPI_tuptable;
        if rc != pg_sys::SPI_OK_SELECT.cast_signed() || table.is_null() || pg_sys::SPI_processed == 0 {
            return None;
        }
        if !A::is_compatible_with(pg_sys::SPI_gettypeid((*table).tupdesc, 1)) {
            return None;
        }
        let mut isnull = false;
        let datum = pg_sys::SPI_getbinval(*(*table).vals, (*table).tupdesc, 1, &raw mut isnull);
        A::from_datum(datum, isnull)
    }
}

/// Whether `value` is valid input for type `typ` (as `pg_input_is_valid`), without raising and
/// without a query.
#[cfg(not(feature = "pg15"))]
pub fn valid_input(value: &str, typ: pg_sys::Oid) -> bool {
    let Ok(text) = std::ffi::CString::new(value) else { return false };
    // SAFETY: the input function is looked up for an existing type and called with a soft error
    // context, so invalid input is reported through it rather than raised; the C string outlives
    // the call, and the datum it may produce is never used.
    unsafe {
        let (mut input, mut ioparam) = (pg_sys::InvalidOid, pg_sys::InvalidOid);
        pg_sys::getTypeInputInfo(typ, &raw mut input, &raw mut ioparam);
        let mut flinfo = pg_sys::FmgrInfo::default();
        pg_sys::fmgr_info(input, &raw mut flinfo);
        let mut escontext = pg_sys::ErrorSaveContext { type_: pg_sys::NodeTag::T_ErrorSaveContext, ..Default::default() };
        let mut result = pg_sys::Datum::from(0);
        pg_sys::InputFunctionCallSafe(
            &raw mut flinfo,
            text.as_ptr().cast_mut(),
            ioparam,
            -1,
            (&raw mut escontext).cast::<pg_sys::Node>(),
            &raw mut result,
        )
    }
}

/// Whether `value` is valid input for type `typ`. Before 16 input functions cannot report an
/// error softly: the call runs in an internal subtransaction, rolled back when it raises, as a
/// PL/pgSQL exception block does.
#[cfg(feature = "pg15")]
pub fn valid_input(value: &str, typ: pg_sys::Oid) -> bool {
    let Ok(text) = std::ffi::CString::new(value) else { return false };
    // SAFETY: as PL/pgSQL's exception blocks: the subtransaction is released or rolled back and
    // the memory context and resource owner restored either way; the error a failed call raised
    // is caught by pgrx and dropped; the C string outlives the call
    unsafe {
        let context = pg_sys::CurrentMemoryContext;
        let owner = pg_sys::CurrentResourceOwner;
        pg_sys::BeginInternalSubTransaction(std::ptr::null());
        pg_sys::CurrentMemoryContext = context;
        let valid = pgrx::PgTryBuilder::new(|| {
            let (mut input, mut ioparam) = (pg_sys::InvalidOid, pg_sys::InvalidOid);
            pg_sys::getTypeInputInfo(typ, &raw mut input, &raw mut ioparam);
            pg_sys::OidInputFunctionCall(input, text.as_ptr().cast_mut(), ioparam, -1);
            true
        })
        .catch_others(|_| false)
        .execute();
        if valid {
            pg_sys::ReleaseCurrentSubTransaction();
        } else {
            pg_sys::RollbackAndReleaseCurrentSubTransaction();
        }
        pg_sys::CurrentMemoryContext = context;
        pg_sys::CurrentResourceOwner = owner;
        valid
    }
}

/// Let Postgres act on a cancel or a timeout that arrived meanwhile (raises its error).
pub fn check_for_interrupts() {
    pgrx::check_for_interrupts!();
}

/// Start a thread that holds nothing of Postgres's (src/remote.rs): with every signal blocked from
/// its first instruction, so a cancel, a timeout or a shutdown signal always reaches the backend.
pub fn spawn_without_signals<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<T>> {
    // SAFETY: sets this thread's signal mask around the spawn (the new thread inherits it) and
    // puts it back; the sets live on this stack.
    unsafe {
        let mut all: libc::sigset_t = std::mem::zeroed();
        let mut before: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&raw mut all);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const all, &raw mut before);
        let spawned = std::thread::Builder::new().name(name.to_owned()).spawn(f);
        libc::pthread_sigmask(libc::SIG_SETMASK, &raw const before, std::ptr::null_mut());
        spawned
    }
}

/// Test only: cancel this backend's statement in `ms`, as pg_cancel_backend does (SIGINT to the
/// backend, from a thread that blocks it, so the backend's own handler takes it).
#[cfg(any(test, feature = "pg_test"))]
pub fn cancel_in(ms: u64) {
    let _ = spawn_without_signals("pg_auth test cancel", move || {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        // SAFETY: sends a signal to this process; touches no memory.
        unsafe { libc::kill(libc::getpid(), libc::SIGINT) };
    });
}

/// Whether a setting currently holds `value` (None: unset or empty), as `config_option` reads
/// it, without copying it: a per-row check compares the request settings it was resolved from.
pub fn config_option_is(name: &CStr, value: Option<&str>) -> bool {
    // SAFETY: as in config_option; the value is only compared, while it is valid.
    let p = unsafe { pg_sys::GetConfigOption(name.as_ptr(), true, false) };
    let current = if p.is_null() {
        &[][..]
    } else {
        // SAFETY: GetConfigOption returns a NUL-terminated string that stays valid until the setting changes.
        unsafe { CStr::from_ptr(p) }.to_bytes()
    };
    match value {
        None => current.is_empty(),
        Some(v) => current == v.as_bytes(),
    }
}

/// The first column of the first row of a read-only query, read with the latest snapshot
/// whatever the isolation level: for configuration kept across transactions
/// (`config_generation`), which must not be loaded from a snapshot older than the invalidation
/// that made it reload.
pub fn read_latest<A: pgrx::FromDatum + pgrx::IntoDatum>(query: &str) -> Option<A> {
    execute(query, &[], Read::Latest)
}

thread_local! {
    /// Bumped when pg_auth's configuration may have changed: a relcache invalidation of
    /// authz_internal.settings, which statement triggers on authz_internal.settings and authz_internal.hierarchies send
    /// (`invalidate_config`). Caches of the configuration hold while it is unchanged.
    static CONFIG_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// authz_internal.settings, once known; until then every invalidation counts
    static CONFIG_REL: std::cell::Cell<pg_sys::Oid> = const { std::cell::Cell::new(pg_sys::InvalidOid) };
    /// Bumped when relationships changed in the engine: a sidecar delivered relationship tables'
    /// changes and sent a relcache invalidation of authz_internal.outbox (`invalidate_answers`). Answers
    /// kept across transactions hold while it is unchanged.
    static ANSWERS_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static ANSWERS_REL: std::cell::Cell<pg_sys::Oid> = const { std::cell::Cell::new(pg_sys::InvalidOid) };
}

/// Listen for configuration changes (once, from _PG_init).
pub fn watch_config() {
    // SAFETY: registers a callback Postgres calls on relcache invalidations; it only bumps a
    // counter, as such callbacks must (no catalog access, no errors).
    unsafe { pg_sys::CacheRegisterRelcacheCallback(Some(config_invalidated), pg_sys::Datum::from(0)) }
}

unsafe extern "C-unwind" fn config_invalidated(_arg: pg_sys::Datum, relid: pg_sys::Oid) {
    let ours = CONFIG_REL.get();
    if relid == pg_sys::InvalidOid || ours == pg_sys::InvalidOid || relid == ours {
        CONFIG_GENERATION.set(CONFIG_GENERATION.get().wrapping_add(1));
        // looked up again on next use: the extension may have been dropped and created again
        CONFIG_REL.set(pg_sys::InvalidOid);
    }
    let answers = ANSWERS_REL.get();
    if relid == pg_sys::InvalidOid || answers == pg_sys::InvalidOid || relid == answers {
        ANSWERS_GENERATION.set(ANSWERS_GENERATION.get().wrapping_add(1));
        ANSWERS_REL.set(pg_sys::InvalidOid);
    }
}

/// The configuration's generation, to take before loading it: an invalidation that arrives
/// while it loads makes the next use load it again.
pub fn config_generation() -> u64 {
    if CONFIG_REL.get() == pg_sys::InvalidOid {
        // SAFETY: syscache lookups by name; missing_ok, so no error when authz_internal does not exist.
        let rel = unsafe {
            let ns = pg_sys::get_namespace_oid(c"authz_internal".as_ptr(), true);
            if ns == pg_sys::InvalidOid { pg_sys::InvalidOid } else { pg_sys::get_relname_relid(c"settings".as_ptr(), ns) }
        };
        CONFIG_REL.set(rel);
    }
    CONFIG_GENERATION.get()
}

/// authz_internal's relation `name`, if the extension is installed.
fn authz_relation(name: &CStr) -> pg_sys::Oid {
    // SAFETY: syscache lookups by name; missing_ok, so no error when authz_internal does not exist
    unsafe {
        let ns = pg_sys::get_namespace_oid(c"authz_internal".as_ptr(), true);
        if ns == pg_sys::InvalidOid { pg_sys::InvalidOid } else { pg_sys::get_relname_relid(name.as_ptr(), ns) }
    }
}

/// The engine's answers' generation: kept answers taken under another one are stale.
pub fn answers_generation() -> u64 {
    if ANSWERS_REL.get() == pg_sys::InvalidOid {
        ANSWERS_REL.set(authz_relation(c"outbox"));
    }
    ANSWERS_GENERATION.get()
}

/// Make every backend forget the answers it kept, when this transaction commits (and this
/// backend now): relationships changed in the engine.
pub fn invalidate_answers() {
    answers_generation();
    let rel = ANSWERS_REL.get();
    if rel != pg_sys::InvalidOid {
        // SAFETY: queues a relcache invalidation for an existing relation
        unsafe { pg_sys::CacheInvalidateRelcacheByRelid(rel) }
    }
    ANSWERS_GENERATION.set(ANSWERS_GENERATION.get().wrapping_add(1));
}

/// Make every backend reload pg_auth's configuration, when this transaction commits (and this
/// backend, from its next command).
pub fn invalidate_config() {
    config_generation();
    let rel = CONFIG_REL.get();
    if rel != pg_sys::InvalidOid {
        // SAFETY: queues a relcache invalidation for an existing relation.
        unsafe { pg_sys::CacheInvalidateRelcacheByRelid(rel) }
    }
    CONFIG_GENERATION.set(CONFIG_GENERATION.get().wrapping_add(1));
}

/// Whether the transaction reads with one snapshot throughout (REPEATABLE READ, SERIALIZABLE):
/// what other transactions commit meanwhile stays invisible to it.
pub fn transaction_snapshot_fixed() -> bool {
    // SAFETY: a backend global, read in the backend's own thread
    unsafe { pg_sys::XactIsoLevel >= pg_sys::XACT_REPEATABLE_READ.cast_signed() }
}
