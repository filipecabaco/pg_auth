//! Client for pg-auth-sidecar (sidecar/query): asks the source-of-truth engine which objects the
//! current subject may access. See sidecar/query/src/protocol.rs for the wire format.
//!
//! One persistent connection per backend, reconnected on failure. Answers are cached for the
//! rest of the transaction, so a statement sees one consistent answer and repeated rows cost no
//! round trips. Any failure is an error to the caller: pg_auth fails closed.

use pgrx::{GucContext, GucFlags, GucRegistry, GucSetting, PgXactCallbackEvent, register_xact_callback};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub static SOCKET: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(Some(c"/tmp/pg_auth.sock"));
/// Long enough for the sidecar to batch-check a few thousand ids (OpenFGA: 1.5–3 ms each) and
/// above its own per-request ENGINE_TIMEOUT_MS (4 s), so engine errors arrive instead of a timeout.
pub static TIMEOUT_MS: GucSetting<i32> = GucSetting::<i32>::new(5000);
pub static PERMISSION_INDEX: GucSetting<bool> = GucSetting::<bool>::new(true);
pub static FILL_WAIT_MS: GucSetting<i32> = GucSetting::<i32>::new(10_000);
/// Ids a statement sends the engine for one subject and permission before it waits for that
/// subject's permission index set to fill: by default about 3 s of engine checks (OpenFGA,
/// ~1.7 ms an id in batches), where a fill of tens of thousands of objects takes about as long
/// and answers the rest of the scan, and every later one, from Postgres.
pub static FILL_WAIT_AFTER: GucSetting<i32> = GucSetting::<i32>::new(2000);
/// Ids a fresh permission index set may hold to be listed:
/// up to it, a subject the engine could not list is listed after all (`col = ANY(...)`, hashed,
/// index scans and partition pruning) instead of checked row by row in AuthzBatch. Off by
/// default: every statement then builds the array and its hash table, which costs a point lookup
/// 1-4 ms for thousands of ids where AuthzBatch's check against the loaded set costs nothing;
/// it pays off only where broad subjects mostly scan.
pub static INDEX_LIST_LIMIT: GucSetting<i32> = GucSetting::<i32>::new(0);

pub fn init() {
    // Superuser-only: whoever controls the socket controls every authorization decision.
    GucRegistry::define_string_guc(
        c"pg_auth.sidecar_socket",
        c"Unix socket of the pg-auth-sidecar that answers authorization questions.",
        c"Superuser-only, since the sidecar's answers decide what every query may read.",
        &SOCKET,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pg_auth.sidecar_timeout_ms",
        c"How long to wait for the sidecar before failing the query.",
        c"",
        &TIMEOUT_MS,
        1,
        60_000,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"pg_auth.permission_index",
        c"Answer subjects with too many objects to list from the permission index (sql/index.sql).",
        c"When off, their rows are checked with the engine, and no sets are filled.",
        &PERMISSION_INDEX,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pg_auth.fill_wait_ms",
        c"How long a statement that checks many rows with the engine waits for the subject's permission index set to fill.",
        c"Once per transaction, after pg_auth.fill_wait_after ids of one subject and permission went to the engine; 0 never waits.",
        &FILL_WAIT_MS,
        0,
        600_000,
        GucContext::Userset,
        GucFlags::UNIT_MS,
    );
    GucRegistry::define_int_guc(
        c"pg_auth.fill_wait_after",
        c"Ids a statement sends the engine for one subject and permission (or a tenth as many requests) before it waits for that subject's permission index set.",
        c"See pg_auth.fill_wait_ms.",
        &FILL_WAIT_AFTER,
        1,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pg_auth.index_list_limit",
        c"Objects a subject's fresh permission index set may have to be listed as ids instead of checked row by row.",
        c"0 (default) never lists them: every statement would build the array, which only pays off where broad subjects mostly scan.",
        &INDEX_LIST_LIMIT,
        0,
        1_000_000,
        GucContext::Userset,
        GucFlags::default(),
    );
}

#[derive(Clone, Debug, PartialEq)]
pub enum Listed {
    /// shared with the loaded set it may come from (a permission index set), not copied
    Ids(Rc<IdSet>),
    /// More objects than the engine lists; rows must be checked individually.
    TooMany,
}

type Key = (String, String, String); // subject, object type, permission

// pg_auth's own queries (ffi keeps their plans), named so a warm-up prepares them (authz.warm)
const INDEXED_IDS: &str = "SELECT authz.indexed_ids($1, $2)";
const HIERARCHY_OF: &str = "SELECT version FROM authz.hierarchy_of($1, $2) WHERE version IS NOT NULL";
const HIERARCHY_EXPAND: &str = "SELECT authz.hierarchy_expand($1, $2)";
const HIERARCHY_DECLARATIONS: &str = "SELECT authz.hierarchy_declarations()";
const RESOURCE_PLAN: &str = "SELECT authz.resource_plan($1, $2)";
const INDEXED_SET: &str = "SELECT array[set_id::text, size::text, filled_at::text] FROM authz.indexed_set($1, $2) WHERE set_id IS NOT NULL";
const INDEXED_LOOKUP: &str = "SELECT authz.indexed_lookup($1, $2)";

/// The queries this module runs, with their parameter types.
pub fn queries() -> Vec<(&'static str, Vec<pgrx::pg_sys::Oid>)> {
    use pgrx::pg_sys::{INT8OID, TEXTARRAYOID, TEXTOID};
    vec![
        (INDEXED_IDS, vec![TEXTOID, TEXTOID]),
        (HIERARCHY_OF, vec![TEXTOID, TEXTOID]),
        (HIERARCHY_EXPAND, vec![TEXTOID, TEXTOID]),
        (HIERARCHY_DECLARATIONS, vec![]),
        (RESOURCE_PLAN, vec![TEXTOID, TEXTOID]),
        (INDEXED_SET, vec![TEXTOID, TEXTOID]),
        (INDEXED_LOOKUP, vec![INT8OID, TEXTARRAYOID]),
    ]
}

/// Load the hierarchy declarations into this backend (authz.warm): how many there are.
pub fn warm_hierarchies() -> usize {
    declared_hierarchies().len()
}

/// Which resources a subject may access, for an engine that decides on their attributes.
pub enum Plan {
    Always,
    Never,
    /// a condition in the protocol's expression language (condition.rs)
    Condition(Value),
    /// a condition outside that language: check each row with its attributes
    Rows,
}

/// Answers kept across transactions, each with when it was kept.
type Kept<K, V> = HashMap<K, (Instant, Rc<V>)>;
/// A declared hierarchy's expansion: (closure version, the direct grants expanded, the expansion).
type Expansion = (i64, Vec<String>, Rc<Loaded>);
/// Declared hierarchies, (type, permission) -> direct permission.
type Declared = Rc<HashMap<(String, String), String>>;

thread_local! {
    static CONN: RefCell<Option<(String, BufReader<UnixStream>)>> = const { RefCell::new(None) };
    /// the request context this transaction's answers were given for (`context_held`)
    static CONTEXT: RefCell<String> = const { RefCell::new(String::new()) };
    static LISTS: RefCell<HashMap<Key, Rc<Listed>>> = RefCell::new(HashMap::new());
    /// list answers kept across transactions, per sidecar socket, until the sidecar's own cache
    /// would stop giving them (its valid_ms)
    static LISTS_KEPT: RefCell<Kept<(String, Key), Listed>> = RefCell::new(HashMap::new());
    /// the last key a per-row check answered from memory (see `Hot`)
    static HOT: RefCell<Option<Hot>> = const { RefCell::new(None) };
    /// counts top-level statements, so per-row checks record a statement's activity once
    static STATEMENT_GEN: Cell<u64> = const { Cell::new(0) };
    static CHECKS: RefCell<HashMap<Key, HashMap<String, bool>>> = RefCell::new(HashMap::new());
    /// plans for this transaction, for an engine that decides on attributes
    static PLANS: RefCell<HashMap<(Key, u64), Rc<Plan>>> = RefCell::new(HashMap::new());
    /// plans kept across transactions while the sidecar's answer is (its valid_ms), per socket
    static PLANS_KEPT: RefCell<Kept<(String, Key, u64), Plan>> = RefCell::new(HashMap::new());
    /// the engine's answers' generation LISTS_KEPT and PLANS_KEPT were kept under
    static KEPT_UNDER: Cell<u64> = const { Cell::new(0) };
    /// a row's answer by its id and attributes, when its plan is to check rows
    static RESOURCE_CHECKS: RefCell<HashMap<(Key, String, String), bool>> = RefCell::new(HashMap::new());
    /// fresh permission index sets found this transaction (None: no fresh set; a fill was requested)
    static INDEXED: RefCell<HashMap<Key, Option<IndexedSet>>> = RefCell::new(HashMap::new());
    /// whole sets this backend loaded, kept across transactions (see `cached_load`)
    static LOADED: RefCell<HashMap<(i64, String), Rc<Loaded>>> = RefCell::new(HashMap::new());
    /// sets this transaction waited for (pg_auth.fill_wait_ms)
    static FILL_WAITED: RefCell<HashSet<Key>> = RefCell::new(HashSet::new());
    /// declared hierarchies' expansions, per subject, kept across transactions: (closure version,
    /// the direct grants expanded, the expansion)
    static HIER_SETS: RefCell<HashMap<Key, Expansion>> = RefCell::new(HashMap::new());
    /// declared hierarchies, (type, permission) -> direct permission, as of a configuration
    /// generation (ffi::config_generation): no query per transaction when none is declared
    static HIER_DECLARED: RefCell<Option<(u64, Declared)>> = const { RefCell::new(None) };
    /// whether index reads take a new snapshot: a set this transaction waited for was committed
    /// after its statement started
    static FRESH_READS: RefCell<bool> = const { RefCell::new(false) };
    /// sets the sidecar was asked to fill this transaction
    static FILL_REQUESTED: RefCell<HashSet<Key>> = RefCell::new(HashSet::new());
    /// what the current top-level statement asked of the engine and the index, per key (advice)
    static STATEMENT: RefCell<HashMap<Key, Activity>> = RefCell::new(HashMap::new());
    static CALLBACK_REGISTERED: RefCell<bool> = const { RefCell::new(false) };
    static STATS: RefCell<Stats> = const { RefCell::new(Stats { calls: 0, ids_checked: 0, sidecar: Duration::ZERO, engine: Duration::ZERO, index_lookups: 0 }) };
}

/// Round trips this backend made to the sidecar since it started (authz.stats()).
#[derive(Clone, Copy)]
pub struct Stats {
    pub calls: u64,
    /// ids sent in check requests
    pub ids_checked: u64,
    /// waiting on the sidecar, engine time included
    pub sidecar: Duration,
    /// of which the sidecar reported waiting on the engine
    pub engine: Duration,
    /// ids answered from the permission index instead of the engine
    pub index_lookups: u64,
}

pub fn stats() -> Stats {
    STATS.with(|s| *s.borrow())
}

/// What one statement needed for one (subject, type, permission): the material for advice.
#[derive(Clone, Default)]
pub struct Activity {
    /// ids sent to the engine to be checked, and the engine time that took
    pub ids_checked: u64,
    /// the check requests that took, one round trip each
    pub check_calls: u64,
    pub check_engine: Duration,
    /// engine time spent listing the subject's objects
    pub list_engine: Duration,
    /// ids answered from the permission index
    pub index_lookups: u64,
    /// a fill was requested: whether the sidecar keeps an index (queued it)
    pub fill_queued: Option<bool>,
    /// the subject has too many objects to list and pg_auth.permission_index is off
    pub index_off: bool,
    /// the statement waited this long for the subject's set to fill, and whether it became fresh
    pub fill_wait: Option<(Duration, bool)>,
}

pub fn statement_reset() {
    flush_hot();
    STATEMENT.with(|s| s.borrow_mut().clear());
    STATEMENT_GEN.set(STATEMENT_GEN.get() + 1);
}

pub fn statement_activity() -> Vec<(Key, Activity)> {
    flush_hot();
    STATEMENT.with(|s| s.borrow().iter().map(|(k, a)| (k.clone(), a.clone())).collect())
}

/// The answer authz.permits gave last, kept so the next row of the same (subject, type,
/// permission) is one set lookup: no key to allocate, no set to clone, nothing to record
/// (authz.permits runs once per row of a write, a locking read or an unbatched scan).
struct Hot {
    key: Key,
    answer: HotAnswer,
    /// the statement whose activity already records this key
    statement: u64,
    /// ids answered from the loaded index set and not counted in the statement's activity yet
    lookups: u64,
}

enum HotAnswer {
    Listed(Rc<Listed>),
    Loaded(Rc<Loaded>),
}

fn set_hot(key: Key, answer: HotAnswer) {
    flush_hot();
    HOT.with(|h| *h.borrow_mut() = Some(Hot { key, answer, statement: STATEMENT_GEN.get(), lookups: 0 }));
}

/// Count the hot key's index lookups in the statement's activity.
fn flush_hot() {
    let pending = HOT.with(|h| h.borrow_mut().as_mut().filter(|h| h.lookups > 0).map(|h| (h.key.clone(), std::mem::take(&mut h.lookups))));
    if let Some((key, n)) = pending {
        record(key, |a| a.index_lookups += n);
    }
}

/// `permits` from the hot answer, when it is for this key.
fn hot_permits(subject: &str, object_type: &str, permission: &str, id: &str) -> Option<bool> {
    let index_on = PERMISSION_INDEX.get();
    let (answer, new_statement) = HOT.with(|h| {
        let mut h = h.borrow_mut();
        let hot = h.as_mut().filter(|h| h.key.0 == subject && h.key.1 == object_type && h.key.2 == permission)?;
        let new_statement = std::mem::replace(&mut hot.statement, STATEMENT_GEN.get()) != STATEMENT_GEN.get();
        let answer = match &hot.answer {
            HotAnswer::Listed(listed) => match &**listed {
                Listed::Ids(ids) => ids.contains(id),
                Listed::TooMany => return None,
            },
            HotAnswer::Loaded(_) if !index_on => return None,
            HotAnswer::Loaded(set) => {
                hot.lookups += 1;
                set.ids.contains(id)
            }
        };
        Some((answer, new_statement.then(|| hot.key.clone())))
    })?;
    if let Some(key) = new_statement {
        // answered from cache or not, this statement used pg_auth for this subject (advice)
        record(key, |_| ());
    }
    if matches!(HOT.with(|h| h.borrow().as_ref().map(|h| matches!(h.answer, HotAnswer::Loaded(_)))), Some(true)) {
        STATS.with(|s| s.borrow_mut().index_lookups += 1);
    }
    Some(answer)
}

fn record(key: Key, f: impl FnOnce(&mut Activity)) {
    STATEMENT.with(|s| f(s.borrow_mut().entry(key).or_default()));
}

fn request_key(request: &Value) -> Option<Key> {
    let field = |k: &str| request.get(k).and_then(Value::as_str).map(str::to_owned);
    Some((field("subject")?, field("type")?, field("permission")?))
}

/// Clear cached answers at the end of the transaction.
fn arm_cache_reset() {
    if CALLBACK_REGISTERED.with(|r| std::mem::replace(&mut *r.borrow_mut(), true)) {
        return;
    }
    let reset = || {
        LISTS.with(|c| c.borrow_mut().clear());
        HOT.with(|h| *h.borrow_mut() = None);
        CHECKS.with(|c| c.borrow_mut().clear());
        PLANS.with(|c| c.borrow_mut().clear());
        RESOURCE_CHECKS.with(|c| c.borrow_mut().clear());
        INDEXED.with(|c| c.borrow_mut().clear());
        FILL_REQUESTED.with(|c| c.borrow_mut().clear());
        FILL_WAITED.with(|c| c.borrow_mut().clear());
        FRESH_READS.with(|f| *f.borrow_mut() = false);
        CALLBACK_REGISTERED.with(|r| *r.borrow_mut() = false);
    };
    register_xact_callback(PgXactCallbackEvent::Commit, reset);
    register_xact_callback(PgXactCallbackEvent::Abort, reset);
}

fn socket_path() -> String {
    SOCKET.get().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

fn connect(path: &str) -> Result<BufReader<UnixStream>, String> {
    let stream = UnixStream::connect(path).map_err(|e| format!("pg_auth sidecar at {path}: {e}"))?;
    let timeout = Some(Duration::from_millis(unsigned(TIMEOUT_MS.get().max(1))));
    stream.set_read_timeout(timeout).map_err(|e| e.to_string())?;
    stream.set_write_timeout(timeout).map_err(|e| e.to_string())?;
    Ok(BufReader::new(stream))
}

/// One request to the sidecar, counted in `stats()`. An engine's error is an error.
fn call(request: &Value) -> Result<Value, String> {
    let started = Instant::now();
    let result = round_trip(request);
    let ids = request.get("ids").and_then(Value::as_array).map_or(0, |a| a.len() as u64);
    let engine = Duration::from_micros(result.as_ref().ok().and_then(|v| v.get("engine_us")).and_then(Value::as_u64).unwrap_or(0));
    STATS.with(|s| {
        let mut s = s.borrow_mut();
        s.calls += 1;
        s.ids_checked += ids;
        s.sidecar += started.elapsed();
        s.engine += engine;
    });
    let result = result.and_then(|v| match v.get("error").and_then(Value::as_str) {
        Some(e) => Err(format!("authorization engine: {e}")),
        None => Ok(v),
    });
    if let Some(key) = request_key(request) {
        let queued = result.as_ref().ok().and_then(|v| v.get("queued")).and_then(Value::as_bool);
        record(key, |a| match request.get("op").and_then(Value::as_str) {
            Some("check") => {
                a.ids_checked += ids;
                a.check_calls += 1;
                a.check_engine += engine;
            }
            Some("list") => a.list_engine += engine,
            Some("fill") => a.fill_queued = Some(queued.unwrap_or(false)),
            _ => {}
        });
    }
    result
}

/// Send one request line and read one response line, once more on a new connection when the
/// sidecar had closed this one (restarted, say); a timeout is not retried.
fn round_trip(request: &Value) -> Result<Value, String> {
    let line = format!("{request}\n");
    let read = |conn: &mut BufReader<UnixStream>| read_answer(conn);
    match exchange(&line, read) {
        Err((true, _)) => exchange(&line, read).map_err(|(_, e)| e),
        result => result.map_err(|(_, e)| e),
    }
}

/// One exchange over this backend's connection. The connection is out of its cell meanwhile and
/// goes back only after a whole answer, so an error, or a cancel unwinding out of `read` (a fill
/// wait), drops it: a late answer is never read as the next request's. Err((broken, message)):
/// broken when the connection was found closed.
fn exchange(line: &str, read: impl FnOnce(&mut BufReader<UnixStream>) -> Result<Vec<u8>, (bool, String)>) -> Result<Value, (bool, String)> {
    let path = socket_path();
    let mut conn = match CONN.with(|c| c.borrow_mut().take()) {
        Some((p, conn)) if p == path => conn,
        _ => connect(&path).map_err(|e| (false, e))?,
    };
    conn.get_mut().write_all(line.as_bytes()).map_err(|e| (broken(&e), e.to_string()))?;
    let answer = read(&mut conn)?;
    let value = serde_json::from_slice(&answer).map_err(|e| (false, format!("bad sidecar response: {e}")))?;
    CONN.with(|c| *c.borrow_mut() = Some((path, conn)));
    Ok(value)
}

fn read_answer(conn: &mut BufReader<UnixStream>) -> Result<Vec<u8>, (bool, String)> {
    let mut answer = Vec::new();
    match conn.read_until(b'\n', &mut answer) {
        Ok(0) => Err((true, "sidecar closed the connection".into())),
        Ok(_) => Ok(answer),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
            Err((false, format!("the sidecar did not answer within pg_auth.sidecar_timeout_ms ({} ms)", TIMEOUT_MS.get())))
        }
        Err(e) => Err((broken(&e), e.to_string())),
    }
}

fn broken(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(e.kind(), BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof)
}

/// Whether the permission index answers: pg_auth.permission_index, and no request context for
/// conditions and caveats (a set is filled for no context in particular, and the time it was
/// filled at may no longer hold).
fn index_on() -> bool {
    PERMISSION_INDEX.get() && crate::subject::request_context().is_none()
}

/// A list or check request, with the request's context for conditions and caveats, if any.
fn asking(mut request: Value) -> Value {
    if let (Some(Value::Object(fields)), Some(r)) = (crate::subject::request_context(), request.as_object_mut()) {
        r.extend(fields);
    }
    request
}

/// This transaction's answers were given for one context: another one (a claim or setting it
/// reads changed meanwhile) asks again. -> the context, as the key of answers kept for it.
fn context_held() -> String {
    let context = crate::subject::request_context().map(|c| c.to_string()).unwrap_or_default();
    let changed = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        if *c == context {
            return false;
        }
        c.clone_from(&context);
        true
    });
    if changed {
        LISTS.with(|c| c.borrow_mut().clear());
        CHECKS.with(|c| c.borrow_mut().clear());
        HOT.with(|h| *h.borrow_mut() = None);
    }
    context
}

/// Answers kept across transactions are dropped once relationships changed in the engine (a
/// sidecar delivered relationship tables' changes: authz.answers_changed).
fn forget_kept_if_relationships_changed() {
    let generation = crate::ffi::answers_generation();
    if KEPT_UNDER.get() != generation {
        LISTS_KEPT.with(|c| c.borrow_mut().clear());
        PLANS_KEPT.with(|c| c.borrow_mut().clear());
        KEPT_UNDER.set(generation);
    }
}

/// Which objects of `object_type` may `subject` access with `permission`?
pub fn list(subject: &str, object_type: &str, permission: &str) -> Result<Rc<Listed>, String> {
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    let context = context_held();
    if let Some(hit) = LISTS.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(hit);
    }
    if let Some(listed) = hierarchy_list(subject, object_type, permission)? {
        arm_cache_reset();
        LISTS.with(|c| c.borrow_mut().insert(key, listed.clone()));
        return Ok(listed);
    }
    // a request's transaction is short: the answer the sidecar would give from its cache anyway
    // spares a round trip, and the plan's of every statement of the next requests; kept per
    // sidecar and per context (another context is another answer)
    let socket = if context.is_empty() { socket_path() } else { format!("{}\0{context}", socket_path()) };
    forget_kept_if_relationships_changed();
    let kept = LISTS_KEPT
        .with(|c| c.borrow().get(&(socket.clone(), key.clone())).filter(|(until, _)| Instant::now() < *until).map(|(_, l)| l.clone()));
    if let Some(hit) = kept {
        let hit = from_index_when_too_many(&key, hit);
        arm_cache_reset();
        LISTS.with(|c| c.borrow_mut().insert(key, hit.clone()));
        return Ok(hit);
    }
    let v = call(&asking(json!({"op": "list", "subject": subject, "permission": permission, "type": object_type})))?;
    let listed = if v.get("too_many").and_then(Value::as_bool) == Some(true) {
        Listed::TooMany
    } else {
        let ids = v.get("ids").and_then(Value::as_array).ok_or("bad sidecar response: no ids")?;
        Listed::Ids(Rc::new(ids.iter().filter_map(Value::as_str).map(str::to_owned).collect()))
    };
    arm_cache_reset();
    let listed = Rc::new(listed);
    if let Some(ms) = v.get("valid_ms").and_then(Value::as_u64).filter(|ms| *ms > 0) {
        LISTS_KEPT.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() >= 10_000 {
                c.retain(|_, (until, _)| Instant::now() < *until);
            }
            c.insert((socket, key.clone()), (Instant::now() + Duration::from_millis(ms), listed.clone()));
        });
    }
    let listed = from_index_when_too_many(&key, listed);
    LISTS.with(|c| c.borrow_mut().insert(key, listed.clone()));
    Ok(listed)
}

/// Objects a hierarchy expansion may list as ids; beyond, it answers as the subject's index set.
const HIERARCHY_LIST_LIMIT: usize = 1000;

/// Objects a fresh permission index set may have to be listed (pg_auth.index_list_limit).
fn index_list_limit() -> usize {
    usize::try_from(INDEX_LIST_LIMIT.get()).unwrap_or(0)
}

/// A subject too broad for the engine to list, with a fresh permission index set of at most
/// pg_auth.index_list_limit ids: those ids, listed (counted as index lookups). Otherwise `listed`.
fn from_index_when_too_many(key: &Key, listed: Rc<Listed>) -> Rc<Listed> {
    if *listed != Listed::TooMany {
        return listed;
    }
    let (subject, object_type, permission) = key;
    match index_set_loaded(subject, object_type, permission, index_list_limit()) {
        Some(set) => Rc::new(Listed::Ids(set.ids.clone())),
        None => listed,
    }
}

/// The subject's fresh permission index set when it has at most `max` objects, loaded (now, if
/// this backend has not yet), and counted as an index lookup. None: no fresh set, or a larger one.
pub fn index_set_loaded(subject: &str, object_type: &str, permission: &str, max: usize) -> Option<Rc<Loaded>> {
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    let mut entry = index_entry(&key).filter(|e| e.size <= max)?;
    let set = if let Some(set) = entry.loaded.clone() {
        set
    } else {
        let all = index_read::<Vec<String>>(INDEXED_IDS, &[object_type.into(), permission.into()])?;
        let set = Rc::new(Loaded::new(all));
        cache_load(entry.id, entry.filled.clone(), set.clone());
        entry.loaded = Some(set.clone());
        INDEXED.with(|c| c.borrow_mut().insert(key, Some(entry)));
        set
    };
    note_index_lookups(subject, object_type, permission, 1);
    Some(set)
}

/// `object_type#permission` inherited along a declared hierarchy (sql/hierarchy.sql): the engine
/// lists only the subject's direct grants, and Postgres expands them down the tree. A small
/// expansion is listed ids (up to HIERARCHY_LIST_LIMIT); a large one is too many to list, and becomes the subject's loaded
/// permission index set (`index_entry`), with no fill and nothing to go stale but the tree's
/// version. None: not a declared hierarchy, or too many direct grants (the engine path).
fn hierarchy_list(subject: &str, object_type: &str, permission: &str) -> Result<Option<Rc<Listed>>, String> {
    let Some(direct) = declared_hierarchies().get(&(object_type.to_string(), permission.to_string())).cloned() else { return Ok(None) };
    // the tree's version as this statement sees it, as the expansion will
    let Some(version) = crate::ffi::read_one::<i64>(HIERARCHY_OF, &[object_type.into(), permission.into()]) else {
        return Ok(None);
    };
    let direct = direct.as_str();
    let Listed::Ids(grants) = &*list(subject, object_type, direct)? else { return Ok(None) };
    let mut grants: Vec<String> = grants.iter().cloned().collect();
    grants.sort();

    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    let kept = HIER_SETS.with(|c| c.borrow().get(&key).filter(|(v, g, _)| *v == version && *g == grants).map(|(_, _, set)| set.clone()));
    let set = if let Some(set) = kept {
        set
    } else {
        let ids = crate::ffi::read_one::<Vec<String>>(HIERARCHY_EXPAND, &[object_type.into(), permission.into()])
            .ok_or_else(|| format!("expanding {object_type}#{permission} for {subject}"))?;
        let set = Rc::new(Loaded::new(ids));
        // bounded as the loaded index sets are
        HIER_SETS.with(|c| {
            let mut c = c.borrow_mut();
            c.remove(&key);
            if c.values().map(|(_, _, s)| s.ids.len()).sum::<usize>() + set.ids.len() > LOADED_MAX {
                c.clear();
            }
            if set.ids.len() <= LOADED_MAX {
                c.insert(key.clone(), (version, grants, set.clone()));
            }
        });
        set
    };
    if set.ids.len() <= HIERARCHY_LIST_LIMIT {
        return Ok(Some(Rc::new(Listed::Ids(set.ids.clone()))));
    }
    arm_cache_reset();
    let entry = IndexedSet { id: HIERARCHY_SET, filled: format!("hierarchy {version}"), size: set.ids.len(), probed: 0, loaded: Some(set) };
    INDEXED.with(|c| c.borrow_mut().insert(key, Some(entry)));
    Ok(Some(Rc::new(Listed::TooMany)))
}

/// The declared hierarchies, loaded once per configuration change.
fn declared_hierarchies() -> Rc<HashMap<(String, String), String>> {
    let generation = crate::ffi::config_generation();
    if let Some(declared) = HIER_DECLARED.with(|d| d.borrow().as_ref().filter(|(g, _)| *g == generation).map(|(_, m)| m.clone())) {
        return declared;
    }
    let flat = crate::ffi::read_latest::<Vec<String>>(HIERARCHY_DECLARATIONS).unwrap_or_default();
    let declared: HashMap<(String, String), String> = flat
        .as_chunks::<3>()
        .0
        .iter()
        .map(|[object_type, permission, direct]| ((object_type.clone(), permission.clone()), direct.clone()))
        .collect();
    let declared = Rc::new(declared);
    HIER_DECLARED.with(|d| *d.borrow_mut() = Some((generation, declared.clone())));
    declared
}

/// The set_id of a hierarchy expansion, which no permission index set has.
const HIERARCHY_SET: i64 = -1;

/// Whether this transaction answers `key` from a hierarchy expansion (not the permission index:
/// pg_auth.permission_index does not apply to it).
fn hierarchy_set(key: &Key) -> bool {
    INDEXED.with(|c| matches!(c.borrow().get(key), Some(Some(s)) if s.id == HIERARCHY_SET))
}

/// A subject's fresh set in the permission index (sql/index.sql), as found this transaction.
#[derive(Clone)]
struct IndexedSet {
    id: i64,
    /// when this fill finished: with `id`, names the set's contents (a refill keeps the id)
    filled: String,
    size: usize,
    /// ids probed one by one so far
    probed: usize,
    /// the whole set, once enough ids were probed that loading it is cheaper
    loaded: Option<Rc<Loaded>>,
}

/// A whole fresh set, loaded: its ids, and those that are integers written the way Postgres
/// writes them, so an integer id column is matched without rendering each row's value as text
/// (an id such as "007" is in `ids` only: no integer column renders that way).
pub struct Loaded {
    pub ids: Rc<IdSet>,
    pub ints: HashSet<i64, std::hash::BuildHasherDefault<IdHasher>>,
}

/// Object ids, as the engine writes them.
pub type IdSet = HashSet<String, std::hash::BuildHasherDefault<IdHasher>>;

/// A hasher for ids (Fx's multiply-rotate, 8 bytes at a time): a scan or a write looks up every
/// row's id, where the standard SipHash costs more than the rest of the check. The ids come from
/// the engine, not from whoever queries, so a hash built against flooding is not needed.
#[derive(Default)]
pub struct IdHasher(u64);

impl std::hash::Hasher for IdHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.write_u64(u64::from_le_bytes(*c));
        }
        for b in rest {
            self.write_u64(u64::from(*b));
        }
    }

    fn write_i64(&mut self, i: i64) {
        self.write_u64(i.cast_unsigned());
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl Loaded {
    fn new(ids: Vec<String>) -> Loaded {
        let ints = ids.iter().filter_map(|s| crate::access::canonical_int::<i64>(s)).collect();
        Loaded { ids: Rc::new(ids.into_iter().collect()), ints }
    }
}

/// Ids kept in loaded sets across transactions, per backend; beyond it older sets are dropped.
const LOADED_MAX: usize = 1_000_000;

/// The loaded contents of a set's fill, kept across transactions: every transaction still finds
/// the subject's fresh set first (`index_entry`), so only contents that are still current are
/// reused, and loading a broad subject's set (~5 ms for 25,000 ids) is paid once per backend
/// rather than once per request.
fn cached_load(id: i64, filled: &str) -> Option<Rc<Loaded>> {
    LOADED.with(|c| c.borrow().get(&(id, filled.to_string())).cloned())
}

fn cache_load(id: i64, filled: String, set: Rc<Loaded>) {
    LOADED.with(|c| {
        let mut c = c.borrow_mut();
        // a refill replaces its earlier contents
        c.retain(|(other, _), _| *other != id);
        if c.values().map(|s| s.ids.len()).sum::<usize>() + set.ids.len() > LOADED_MAX {
            c.clear();
        }
        if set.ids.len() <= LOADED_MAX {
            c.insert((id, filled), set);
        }
    });
}

/// A probe costs several times more per id than loading costs per element: load the whole set
/// once a transaction has probed this fraction of it (a primary key lookup never does).
const LOAD_AFTER: usize = 8;

/// Whether the permission index can answer the subject: off, a fresh set of this size, or none
/// yet. For authz.explain.
pub enum IndexStatus {
    Off,
    Fresh(usize),
    Missing,
}

pub fn index_status(subject: &str, object_type: &str, permission: &str) -> IndexStatus {
    if !index_on() {
        return IndexStatus::Off;
    }
    match index_entry(&(subject.to_string(), object_type.to_string(), permission.to_string())) {
        Some(set) => IndexStatus::Fresh(set.size),
        None => IndexStatus::Missing,
    }
}

// ---- breadth of the subjects cached plans were built for ------------------------------------
//
// A cached (generic) plan is built for the subject who planned it: how many objects it may
// access shapes the plan (a hash join over a listed-ids bitmap scan for a few ids, an ordered
// index walk for many). Postgres does not know the plan depends on the subject, so a plan built
// for a narrow subject would be reused by a broad one, or the reverse. The planner records each
// (type, permission)'s breadth class; when a later request's subject falls in another class,
// this backend's cached plans are discarded (rewrite/statements.rs) and rebuilt for it.

thread_local! {
    /// (type, permission) -> breadth class of the subject the cached plans were built for
    static PLANNED_BREADTH: RefCell<HashMap<(String, String), u8>> = RefCell::new(HashMap::new());
    /// the plan skeleton each compiled filter of a cached plan was built from (rewrite/compile.rs)
    static PLANNED_SHAPES: RefCell<HashMap<(String, String), String>> = RefCell::new(HashMap::new());
    /// the principal (its hash) and time the planned shapes were last found unchanged
    static SHAPES_CHECKED: Cell<Option<(u64, Instant)>> = const { Cell::new(None) };
    /// the subject the classes were last compared for (None: no subject)
    static BREADTH_SUBJECT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Orders of magnitude of a subject's set, as far as plan shapes go.
fn breadth(listed: Option<&Listed>) -> u8 {
    match listed {
        None => 0,
        Some(Listed::Ids(ids)) if ids.len() <= 4 => 0,
        Some(Listed::Ids(ids)) if ids.len() <= 64 => 1,
        Some(Listed::Ids(_)) => 2,
        Some(Listed::TooMany) => 3,
    }
}

/// A plan is being built for `subject` (None: no subject, nothing allowed) with this answer.
pub fn note_planned(subject: Option<&str>, object_type: &str, permission: &str, listed: Option<&Listed>) {
    let class = breadth(listed);
    PLANNED_BREADTH.with(|p| {
        p.borrow_mut().entry((object_type.to_string(), permission.to_string())).or_insert(class);
    });
    BREADTH_SUBJECT.with(|s| *s.borrow_mut() = subject.map(str::to_string));
}

/// Whether any plan recorded a breadth class or a compiled shape (only then is the subject worth
/// resolving).
pub fn breadth_planned() -> bool {
    PLANNED_BREADTH.with(|p| !p.borrow().is_empty()) || PLANNED_SHAPES.with(|p| !p.borrow().is_empty())
}

/// Whether a cached plan compiled a filter from a plan skeleton.
pub fn shapes_planned() -> bool {
    PLANNED_SHAPES.with(|p| !p.borrow().is_empty())
}

/// A cached plan compiled a filter from this plan skeleton.
pub fn note_planned_shape(object_type: &str, permission: &str, shape: &str) {
    PLANNED_SHAPES.with(|p| {
        p.borrow_mut().entry((object_type.to_string(), permission.to_string())).or_insert_with(|| shape.to_string());
    });
    SHAPES_CHECKED.set(None);
}

/// The current subject's plan skeleton: from memory while the subject is resolved, else through
/// the security definer function (the caller never reads authz.settings).
pub fn current_shape(object_type: &str, permission: &str) -> Option<String> {
    if crate::subject::resolved() {
        return crate::access::resource_plan_now(object_type, permission);
    }
    crate::ffi::read_one::<String>(RESOURCE_PLAN, &[object_type.into(), permission.into()])
}

/// Whether the current subject's plan has another skeleton than a compiled filter of the cached
/// plans: then the caller discards them. Compared at every check, since a principal's roles may
/// change while its id stays.
pub fn shapes_changed() -> bool {
    // the same subject as at the last check, which found them unchanged: plans change with a
    // policy only, which the sidecar answers anew after its TTL
    let generation = crate::subject::resolved().then(crate::subject::principal_key);
    if let (Some(g), Some((checked, at))) = (generation, SHAPES_CHECKED.get())
        && g == checked
        && at.elapsed() < Duration::from_millis(500)
    {
        return false;
    }
    let planned: Vec<((String, String), String)> =
        PLANNED_SHAPES.with(|p| p.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let changed = planned.iter().any(|((object_type, permission), shape)| {
        // no subject: every compiled filter answers it; no plan to compare (the sidecar
        // unreachable): rebuilt, to be safe
        let current = current_shape(object_type, permission);
        current.is_none_or(|c| c != *shape && c != r#"{"plan":"none"}"#)
    });
    SHAPES_CHECKED.set(match (changed, generation) {
        (false, Some(g)) => Some((g, Instant::now())),
        _ => None,
    });
    changed
}

/// The cached plans were discarded: what they were built for starts over.
pub fn forget_planned() {
    PLANNED_BREADTH.with(|p| p.borrow_mut().clear());
    PLANNED_SHAPES.with(|p| p.borrow_mut().clear());
    SHAPES_CHECKED.set(None);
}

/// Whether `subject` falls in another breadth class than the cached plans were built for, for
/// any (type, permission) they use (its caller checks `breadth_planned` first): then the caller
/// discards them, and the classes start over.
/// Answered without the sidecar while the subject stays the same; an error counts as a change.
pub fn breadth_changed(subject: Option<&str>) -> bool {
    // no subject: nothing is allowed whatever the plan, and the next subject is compared then
    let Some(subject) = subject else { return false };
    if BREADTH_SUBJECT.with(|s| s.borrow().as_deref() == Some(subject)) {
        return false;
    }
    let planned: Vec<((String, String), u8)> = PLANNED_BREADTH.with(|p| p.borrow().iter().map(|(k, v)| (k.clone(), *v)).collect());
    let changed = planned
        .iter()
        .any(|((object_type, permission), class)| list(subject, object_type, permission).map_or(true, |l| breadth(Some(&l)) != *class));
    BREADTH_SUBJECT.with(|s| *s.borrow_mut() = Some(subject.to_string()));
    if changed {
        PLANNED_BREADTH.with(|p| p.borrow_mut().clear());
    }
    changed
}

/// The subject's fresh set in the permission index, found once per transaction. The index
/// functions resolve the current subject themselves; the key's subject is that subject.
fn index_entry(key: &Key) -> Option<IndexedSet> {
    if !index_on() && !hierarchy_set(key) {
        return None;
    }
    if !INDEXED.with(|c| c.borrow().contains_key(key)) {
        let (_, object_type, permission) = key;
        let found = index_read::<Vec<String>>(INDEXED_SET, &[object_type.as_str().into(), permission.as_str().into()]);
        arm_cache_reset();
        let entry = found.and_then(|row| match row.as_slice() {
            [id, size, filled] => Some(IndexedSet {
                id: id.parse().ok()?,
                size: size.parse().ok()?,
                probed: 0,
                loaded: cached_load(id.parse().ok()?, filled),
                filled: filled.clone(),
            }),
            _ => None,
        });
        INDEXED.with(|c| c.borrow_mut().insert(key.clone(), entry));
    }
    INDEXED.with(|c| c.borrow().get(key).cloned().flatten())
}

/// Whether the subject's fresh index set contains each of `ids` (None for a missing id), or None
/// when there is no fresh set: then the sidecar is asked to fill it (once per transaction, best
/// effort) and the caller asks the engine.
fn from_index(subject: &str, object_type: &str, permission: &str, ids: &[Option<String>]) -> Option<Vec<bool>> {
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    if !index_on() && !hierarchy_set(&key) {
        record(key, |a| a.index_off = true);
        return None;
    }
    let Some(mut set) = index_entry(&key) else {
        if FILL_REQUESTED.with(|c| c.borrow_mut().insert(key)) {
            let _ = call(&json!({"op": "fill", "subject": subject, "permission": permission, "type": object_type}));
        }
        return None;
    };

    let contains = |allowed: &IdSet| answers(ids, |i| allowed.contains(i));
    let answers = if let Some(all) = set.loaded.clone() {
        Some(contains(&all.ids))
    } else {
        let distinct: Vec<String> = ids.iter().flatten().cloned().collect::<HashSet<_>>().into_iter().collect();
        if set.probed + distinct.len() > (set.size / LOAD_AFTER).max(64) {
            let all = index_read::<Vec<String>>(INDEXED_IDS, &[object_type.into(), permission.into()]);
            all.map(|all| {
                let all = Rc::new(Loaded::new(all));
                cache_load(set.id, set.filled.clone(), all.clone());
                set.loaded = Some(all.clone());
                contains(&all.ids)
            })
        } else {
            let found = index_read::<Vec<String>>(INDEXED_LOOKUP, &[set.id.into(), distinct.clone().into()]);
            found.map(|found| {
                set.probed += distinct.len();
                contains(&found.into_iter().collect())
            })
        }
    };
    // stale or refilling since it was found: the engine answers from now on
    let Some(answers) = answers else {
        INDEXED.with(|c| c.borrow_mut().insert(key, None));
        return None;
    };
    INDEXED.with(|c| c.borrow_mut().insert(key, Some(set)));
    note_index_lookups(subject, object_type, permission, ids.len());
    Some(answers)
}

/// The subject's whole fresh set, when it is loaded (this transaction, or earlier by this
/// backend): scans then check their rows against it themselves, one by one.
pub fn loaded(subject: &str, object_type: &str, permission: &str) -> Option<Rc<Loaded>> {
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    if !index_on() && !hierarchy_set(&key) {
        return None;
    }
    index_entry(&key)?.loaded
}

/// Forget this transaction's answers (tests: as if a new transaction began).
#[cfg(any(test, feature = "pg_test"))]
pub fn forget_answers() {
    LISTS.with(|c| c.borrow_mut().clear());
    INDEXED.with(|c| c.borrow_mut().clear());
    HOT.with(|h| *h.borrow_mut() = None);
}

/// A read of the permission index: with the statement's snapshot, or a new one once this
/// transaction waited for a set that was committed meanwhile (`wait_for_fill`).
fn index_read<A: pgrx::FromDatum + pgrx::IntoDatum>(query: &str, args: &[pgrx::datum::DatumWithOid]) -> Option<A> {
    if FRESH_READS.with(|f| *f.borrow()) { crate::ffi::read_one_fresh(query, args) } else { crate::ffi::read_one(query, args) }
}

/// A statement that has sent the engine pg_auth.fill_wait_after ids of this subject is heavy: wait for
/// the subject's set to fill (once per transaction, up to pg_auth.fill_wait_ms) so the rest of
/// it, and every later statement, is answered from Postgres. -> whether a fresh set exists now.
/// Under REPEATABLE READ the set stays invisible to the transaction, and the engine answers.
fn wait_for_fill(subject: &str, object_type: &str, permission: &str) -> bool {
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    let wait = FILL_WAIT_MS.get();
    if wait <= 0 || !index_on() {
        return false;
    }
    // ids, or round trips: checked row by row (a write), each id is one, so a tenth as many
    let (ids, calls) = STATEMENT.with(|s| s.borrow().get(&key).map_or((0, 0), |a| (a.ids_checked, a.check_calls)));
    let after = unsigned(FILL_WAIT_AFTER.get());
    if ids < after && calls < after.div_ceil(10) {
        return false;
    }
    if !FILL_WAITED.with(|w| w.borrow_mut().insert(key.clone())) {
        return false;
    }
    arm_cache_reset();
    let started = Instant::now();
    let request = json!({"op": "fill", "subject": subject, "permission": permission, "type": object_type, "wait_ms": wait});
    let fresh = round_trip_waiting(&request, Duration::from_millis(unsigned(wait)))
        .ok()
        .and_then(|v| v.get("fresh").and_then(Value::as_bool))
        .unwrap_or(false);
    record(key.clone(), |a| a.fill_wait = Some((started.elapsed(), fresh)));
    if fresh {
        // found again, with a snapshot that sees it
        INDEXED.with(|c| c.borrow_mut().remove(&key));
        FRESH_READS.with(|f| *f.borrow_mut() = true);
    }
    fresh
}

/// A request the sidecar answers after up to `limit` (a fill wait). Reads in short slices so a
/// cancel or statement_timeout still applies meanwhile (and drops the connection, see exchange).
fn round_trip_waiting(request: &Value, limit: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + limit + Duration::from_millis(unsigned(TIMEOUT_MS.get().max(1)));
    let read = |conn: &mut BufReader<UnixStream>| {
        let timeout = |conn: &BufReader<UnixStream>, ms: u64| {
            conn.get_ref().set_read_timeout(Some(Duration::from_millis(ms))).map_err(|e| (false, e.to_string()))
        };
        timeout(conn, 100)?;
        let mut answer = Vec::new();
        loop {
            match conn.read_until(b'\n', &mut answer) {
                Ok(0) => return Err((true, "sidecar closed the connection".into())),
                Ok(_) => break,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    if Instant::now() > deadline {
                        return Err((false, "sidecar did not answer the fill wait in time".into()));
                    }
                    crate::ffi::check_for_interrupts();
                }
                Err(e) => return Err((broken(&e), e.to_string())),
            }
        }
        timeout(conn, unsigned(TIMEOUT_MS.get().max(1)))?;
        Ok(answer)
    };
    exchange(&format!("{request}\n"), read).map_err(|(_, e)| e)
}

/// `n` ids were answered from the permission index (authz.stats(), advice).
pub fn note_index_lookups(subject: &str, object_type: &str, permission: &str, n: usize) {
    STATS.with(|s| s.borrow_mut().index_lookups += n as u64);
    record((subject.to_string(), object_type.to_string(), permission.to_string()), |a| a.index_lookups += n as u64);
}

/// May `subject` access object `id`? Uses the listed set when there is one, then the
/// permission index, otherwise asks.
pub fn permits(subject: &str, object_type: &str, permission: &str, id: &str) -> Result<bool, String> {
    if let Some(answer) = hot_permits(subject, object_type, permission, id) {
        return Ok(answer);
    }
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    // answered from cache or not, this statement used pg_auth for this subject (advice)
    record(key.clone(), |_| ());
    let listed = list(subject, object_type, permission)?;
    if let Listed::Ids(ids) = &*listed {
        let answer = ids.contains(id);
        set_hot(key, HotAnswer::Listed(listed.clone()));
        return Ok(answer);
    }
    // a statement that checked many rows one by one (a large write) waits for the subject's set,
    // as a batched read does (wait_for_fill), and answers the rest from it
    let indexed = from_index(subject, object_type, permission, &[Some(id.to_string())]).or_else(|| {
        wait_for_fill(subject, object_type, permission)
            .then(|| from_index(subject, object_type, permission, &[Some(id.to_string())]))
            .flatten()
    });
    if let Some(answer) = indexed {
        if let Some(set) = loaded(subject, object_type, permission) {
            set_hot(key, HotAnswer::Loaded(set));
        }
        return Ok(answer[0]);
    }
    Ok(!filter(subject, object_type, permission, vec![id.to_string()])?.is_empty())
}

/// May `subject` access each of `ids`? One answer per id, in order; a missing id is denied. Uses
/// the listed set when there is one (no round trip beyond the list), then the permission index,
/// otherwise checks the distinct ids in batches. For scans that check rows in batches (pg/exec.rs).
pub fn permits_many(subject: &str, object_type: &str, permission: &str, ids: &[Option<String>]) -> Result<Vec<bool>, String> {
    record((subject.to_string(), object_type.to_string(), permission.to_string()), |_| ());
    let listed = list(subject, object_type, permission)?;
    if let Listed::Ids(set) = &*listed {
        return Ok(answers(ids, |i| set.contains(i)));
    }
    if let Some(answers) = from_index(subject, object_type, permission, ids) {
        return Ok(answers);
    }
    if wait_for_fill(subject, object_type, permission)
        && let Some(answers) = from_index(subject, object_type, permission, ids)
    {
        return Ok(answers);
    }
    let distinct: HashSet<String> = ids.iter().flatten().cloned().collect();
    let allowed: HashSet<String> = filter(subject, object_type, permission, distinct.into_iter().collect())?.into_iter().collect();
    Ok(answers(ids, |i| allowed.contains(i)))
}

/// One answer per id, in order; a missing id is denied.
fn answers(ids: &[Option<String>], allowed: impl Fn(&str) -> bool) -> Vec<bool> {
    ids.iter().map(|id| id.as_deref().is_some_and(&allowed)).collect()
}

/// Requests per round trip in `filter`; the sidecar splits them further for the engine.
const CHECK_CHUNK: usize = 1000;

/// The subset of `ids` that `subject` may access, asking in batches. Missing answers are denials.
pub fn filter(subject: &str, object_type: &str, permission: &str, ids: Vec<String>) -> Result<Vec<String>, String> {
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    context_held();
    let (mut allowed, unknown): (Vec<String>, Vec<String>) = CHECKS.with(|c| {
        let c = c.borrow();
        let known = c.get(&key);
        let (known_ids, unknown) = ids.into_iter().partition::<Vec<_>, _>(|id| known.is_some_and(|k| k.contains_key(id)));
        (known_ids.into_iter().filter(|id| known.is_some_and(|k| k[id])).collect(), unknown)
    });
    for chunk in unknown.chunks(CHECK_CHUNK) {
        let v = call(&asking(json!({"op": "check", "subject": subject, "permission": permission, "type": object_type, "ids": chunk})))?;
        let answers = v.get("allowed").and_then(Value::as_array).ok_or("bad sidecar response: no allowed")?;
        arm_cache_reset();
        CHECKS.with(|c| {
            let mut c = c.borrow_mut();
            let known = c.entry(key.clone()).or_default();
            for (i, id) in chunk.iter().enumerate() {
                let ok = answers.get(i).and_then(Value::as_bool).unwrap_or(false);
                known.insert(id.clone(), ok);
                if ok {
                    allowed.push(id.clone());
                }
            }
        });
    }
    Ok(allowed)
}

/// The subject's plan for `object_type` and `permission`, kept for the transaction, and across
/// transactions while the sidecar's answer is valid, per principal (`generation`: the hash of its
/// content, subject::principal_key); `principal` is resolved only when the plan is asked for.
pub fn plan(
    subject: &str,
    object_type: &str,
    permission: &str,
    generation: u64,
    principal: impl FnOnce() -> Option<Value>,
) -> Result<Rc<Plan>, String> {
    let key = (subject.to_string(), object_type.to_string(), permission.to_string());
    if let Some(plan) = PLANS.with(|c| c.borrow().get(&(key.clone(), generation)).cloned()) {
        return Ok(plan);
    }
    record(key.clone(), |_| ());
    // a request's transaction is short: the plan the sidecar would answer from its cache anyway
    // spares a round trip at every statement's planning
    let socket = socket_path();
    forget_kept_if_relationships_changed();
    let kept = PLANS_KEPT.with(|c| {
        c.borrow().get(&(socket.clone(), key.clone(), generation)).filter(|(until, _)| Instant::now() < *until).map(|(_, p)| p.clone())
    });
    if let Some(plan) = kept {
        arm_cache_reset();
        PLANS.with(|c| c.borrow_mut().insert((key, generation), plan.clone()));
        return Ok(plan);
    }
    let v =
        call(&asking(json!({"op": "plan", "subject": subject, "permission": permission, "type": object_type, "principal": principal()})))?;
    let plan = match v.get("plan").and_then(Value::as_str) {
        Some("always") => Plan::Always,
        Some("never") => Plan::Never,
        Some("rows") => Plan::Rows,
        Some("condition") => Plan::Condition(v.get("condition").cloned().ok_or("bad sidecar response: no condition")?),
        _ => return Err("bad sidecar response: no plan".into()),
    };
    arm_cache_reset();
    let plan = Rc::new(plan);
    if let Some(ms) = v.get("valid_ms").and_then(Value::as_u64).filter(|ms| *ms > 0) {
        PLANS_KEPT.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() >= 10_000 {
                c.retain(|_, (until, _)| Instant::now() < *until);
            }
            c.insert((socket, key.clone(), generation), (Instant::now() + Duration::from_millis(ms), plan.clone()));
        });
    }
    PLANS.with(|c| c.borrow_mut().insert((key, generation), plan.clone()));
    Ok(plan)
}

/// May `subject` access the row with this id and these attributes? Asks the engine; kept for
/// the transaction.
pub fn check_resource(
    subject: &str,
    object_type: &str,
    permission: &str,
    id: &str,
    attrs: &Value,
    principal: Option<&Value>,
) -> Result<bool, String> {
    let key = ((subject.to_string(), object_type.to_string(), permission.to_string()), id.to_string(), attrs.to_string());
    if let Some(hit) = RESOURCE_CHECKS.with(|c| c.borrow().get(&key).copied()) {
        return Ok(hit);
    }
    let v = call(&asking(
        json!({"op": "check", "subject": subject, "permission": permission, "type": object_type, "ids": [id], "attrs": [attrs], "principal": principal}),
    ))?;
    let allowed = v.get("allowed").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_bool).unwrap_or(false);
    arm_cache_reset();
    RESOURCE_CHECKS.with(|c| c.borrow_mut().insert(key, allowed));
    Ok(allowed)
}

/// A setting in milliseconds or counts as unsigned: negative is zero.
fn unsigned(n: i32) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// The engine behind the sidecar.
pub fn ping() -> Result<String, String> {
    let v = call(&json!({"op": "ping"}))?;
    v.get("engine").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| "bad sidecar response: no engine".into())
}
