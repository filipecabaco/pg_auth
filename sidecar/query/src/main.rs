//! pg_auth query sidecar: answers "which objects may this subject access" for the Postgres
//! extension, asking the source-of-truth engine (OpenFGA or SpiceDB; engine.rs). Runs next to
//! Postgres and listens on a local Unix socket; see protocol.rs for the wire format.
//!
//! Configuration (environment). The engine is Cerbos when CERBOS_URL is set, SpiceDB when
//! SPICEDB_URL is, else OpenFGA:
//!   OPENFGA_URL, OPENFGA_STORE_ID          required for OpenFGA
//!   OPENFGA_MODEL_ID, OPENFGA_API_TOKEN    optional
//!   OPENFGA_CONSISTENCY                    optional, e.g. HIGHER_CONSISTENCY
//!   SPICEDB_URL, SPICEDB_TOKEN             SpiceDB's HTTP gateway (--http-enabled) and its
//!                                          preshared key
//!   SPICEDB_CONSISTENCY                    minimize_latency (default) or fully_consistent
//!   CERBOS_URL                             Cerbos's HTTP API
//!   PG_AUTH_SOCKET                         default /tmp/pg_auth.sock
//!   LIST_LIMIT                             default 1000 (OpenFGA's listObjectsMaxResults,
//!                                          SpiceDB's max-lookup-resources-limit)
//!   CHECK_CONCURRENCY                      default 8: BatchCheck (OpenFGA, 50 ids each) or
//!                                          CheckBulkPermissions (SpiceDB, 1000 ids each) calls
//!                                          in flight per check request
//!   CACHE_TTL_MS                           default 1000; 0 disables caching. A list or plan past
//!                                          half its TTL is refreshed in the background, so a subject
//!                                          asking steadily never waits on the engine
//!   REFRESH_ACTIVE_MS                      default 30000: lists and plans used within this long are
//!                                          refreshed before they expire even when nobody asks, so a
//!                                          subject back after a pause does not wait on the engine
//!                                          either; 0 refreshes only what is asked for. Costs about
//!                                          two engine calls per second per active list or plan
//!   REFRESH_CONCURRENCY                    default 8: background refreshes in flight
//!   ENGINE_TIMEOUT_MS                      default 4000; above OPENFGA_LIST_DEADLINE_MS, below the
//!                                          extension's pg_auth.sidecar_timeout_ms (5000)
//!   OPENFGA_LIST_DEADLINE_MS               default 3000: OpenFGA's listObjectsDeadline
//!   PG_AUTH_DATABASE_URL                   optional: enables the permission index (sql/index.sql)
//!   INDEX_POLL_MS                          default 500: how often OpenFGA's change feed is read
//!                                          (SpiceDB's Watch streams), and the retry interval; also
//!                                          how often the relationship tables' outbox is read when
//!                                          no NOTIFY says it changed

#![forbid(unsafe_code)]

mod cerbos;
mod engine;
mod index;
mod openfga;
mod openfga_dsl;
mod outbox;
mod protocol;
mod rules;
mod spicedb;

use cerbos::{Cerbos, Plan};
use engine::{Context, Engine, Key, Listed};
use index::Index;
use openfga::OpenFga;
use protocol::{Request, Response};
use spicedb::SpiceDb;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Answers kept per cache; a full cache is emptied (its entries live a TTL, a second by default).
const CACHE_MAX_ENTRIES: usize = 100_000;

struct State {
    engine: Arc<Engine>,
    index: Option<Index>,
    list_limit: usize,
    check_concurrency: usize,
    ttl: Duration,
    /// how recently a list or plan must have been used to be refreshed in the background
    active: Duration,
    refresh_concurrency: usize,
    /// lists per subject and request context (as JSON; "" for none)
    lists: Cache<(Key, String), Listed>,
    /// plans per subject and principal (its roles and attributes, as JSON; "" for none)
    plans: Cache<(Key, String), Plan>,
    /// lists and plans being refreshed ahead of their expiry
    refreshing: Mutex<HashSet<Refresh>>,
    checks: Checks,
    /// print one line per request (REQUEST_LOG=1): what Postgres asked and what it was told
    log: bool,
}

/// What a background refresh asks the engine again.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Refresh {
    List((Key, String)),
    Plan((Key, String)),
}

/// Answers kept for a TTL, each with when it was fetched and when a request last used it.
struct Cache<K, V> {
    ttl: Duration,
    entries: Mutex<HashMap<K, Cached<V>>>,
}

struct Cached<V> {
    at: Instant,
    used: Instant,
    value: V,
}

impl<K: Eq + Hash + Clone, V: Clone> Cache<K, V> {
    fn new(ttl: Duration) -> Self {
        Cache { ttl, entries: Mutex::new(HashMap::new()) }
    }

    /// Forget every answer.
    fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }

    /// A request's cached answer and how many milliseconds it stays fresh.
    fn get(&self, key: &K) -> Option<(V, u64)> {
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.get_mut(key).filter(|e| e.at.elapsed() < self.ttl)?;
        entry.used = Instant::now();
        Some((entry.value.clone(), u64::try_from(self.ttl.saturating_sub(entry.at.elapsed()).as_millis()).unwrap_or(u64::MAX)))
    }

    /// A request's answer from the engine.
    fn put(&self, key: K, value: V) {
        if self.ttl.is_zero() {
            return;
        }
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= CACHE_MAX_ENTRIES {
                entries.clear();
            }
            let now = Instant::now();
            entries.insert(key, Cached { at: now, used: now, value });
        }
    }

    /// A refreshed answer: replaces the cached one, which keeps when it was last used (a refresh
    /// is not a use). Dropped if the cached one is gone.
    fn refreshed(&self, key: &K, value: V) {
        if let Some(entry) = self.entries.lock().ok().as_mut().and_then(|e| e.get_mut(key)) {
            entry.at = Instant::now();
            entry.value = value;
        }
    }

    /// The answers used within `active` that are past half their TTL, oldest first; answers
    /// neither fresh nor active are dropped.
    fn due(&self, active: Duration) -> Vec<K> {
        let Ok(mut entries) = self.entries.lock() else { return Vec::new() };
        entries.retain(|_, e| e.at.elapsed() < self.ttl || e.used.elapsed() < active);
        let mut due: Vec<_> = entries
            .iter()
            .filter(|(_, e)| e.at.elapsed() >= self.ttl / 2 && e.used.elapsed() < active)
            .map(|(k, e)| (e.at, k.clone()))
            .collect();
        due.sort_by_key(|(at, _)| *at);
        due.into_iter().map(|(_, k)| k).collect()
    }
}

/// Check answers, per subject, type and permission: one lock per request, and a string kept only
/// for a new answer.
#[derive(Default)]
struct Checks {
    /// answers inserted since the cache was last emptied, and the answers
    entries: Mutex<(usize, CheckAnswers)>,
}

type CheckAnswers = HashMap<Key, HashMap<String, (Instant, bool)>>;

impl Checks {
    /// Forget every answer (relationships changed).
    fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            *entries = (0, CheckAnswers::default());
        }
    }

    fn get(&self, key: &Key, ids: &[String], ttl: Duration) -> Vec<Option<bool>> {
        let entries = self.entries.lock().ok();
        let answers = entries.as_ref().and_then(|e| e.1.get(key));
        ids.iter().map(|id| answers.and_then(|a| a.get(id)).filter(|(at, _)| at.elapsed() < ttl).map(|(_, allowed)| *allowed)).collect()
    }

    fn put<'a>(&self, key: &Key, answers: impl Iterator<Item = (&'a String, bool)>) {
        let Ok(mut entries) = self.entries.lock() else { return };
        if entries.0 >= CACHE_MAX_ENTRIES {
            *entries = Default::default();
        }
        let now = Instant::now();
        let (count, map) = &mut *entries;
        let mine = map.entry(key.clone()).or_default();
        for (id, allowed) in answers {
            *count += 1;
            mine.insert(id.clone(), (now, allowed));
        }
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_num(name: &str, default: u64) -> u64 {
    env(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("pg-auth-sidecar: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let timeout = Duration::from_millis(env_num("ENGINE_TIMEOUT_MS", 4000));
    let concurrency = usize::try_from(env_num("CHECK_CONCURRENCY", 8)).unwrap_or(usize::MAX);
    let poll = Duration::from_millis(env_num("INDEX_POLL_MS", 500));
    let (engine, describe) = match (env("CERBOS_URL"), env("SPICEDB_URL")) {
        (Some(url), _) => {
            let admin = env("CERBOS_ADMIN_USER").zip(env("CERBOS_ADMIN_PASSWORD"));
            (Engine::Cerbos(Cerbos::new(&url, timeout, admin)?), format!("cerbos {url}"))
        }
        (None, Some(url)) => {
            let token = env("SPICEDB_TOKEN").ok_or("SPICEDB_TOKEN is required")?;
            let engine = SpiceDb::new(&url, &token, env("SPICEDB_CONSISTENCY").as_deref(), timeout)?;
            (Engine::SpiceDb(engine), format!("spicedb {url}"))
        }
        (None, None) => {
            let url = env("OPENFGA_URL").ok_or("OPENFGA_URL (or SPICEDB_URL, CERBOS_URL) is required")?;
            let store = env("OPENFGA_STORE_ID").ok_or("OPENFGA_STORE_ID is required")?;
            let engine = OpenFga::new(
                &url,
                &store,
                env("OPENFGA_MODEL_ID"),
                env("OPENFGA_API_TOKEN"),
                env("OPENFGA_CONSISTENCY"),
                timeout,
                Duration::from_millis(env_num("OPENFGA_LIST_DEADLINE_MS", 3000)),
            )?;
            (Engine::OpenFga(engine), format!("openfga {url}, store {store}"))
        }
    };
    let engine = Arc::new(engine);
    let index = env("PG_AUTH_DATABASE_URL").map(|url| {
        eprintln!("pg-auth-sidecar: permission index on, following the change feed");
        Index::start(url, engine.clone(), poll)
    });
    let ttl = Duration::from_millis(env_num("CACHE_TTL_MS", 1000));
    let state = Arc::new(State {
        engine,
        index,
        list_limit: usize::try_from(env_num("LIST_LIMIT", 1000)).unwrap_or(usize::MAX),
        check_concurrency: concurrency,
        ttl,
        active: Duration::from_millis(env_num("REFRESH_ACTIVE_MS", 30_000)),
        refresh_concurrency: usize::try_from(env_num("REFRESH_CONCURRENCY", 8).max(1)).unwrap_or(usize::MAX),
        lists: Cache::new(ttl),
        plans: Cache::new(ttl),
        refreshing: Mutex::new(HashSet::new()),
        checks: Checks::default(),
        log: env("REQUEST_LOG").is_some_and(|v| v != "0"),
    });
    if !state.ttl.is_zero() && !state.active.is_zero() {
        tokio::spawn(refresher(state.clone()));
    }
    // relationship tables: the changes applications write, delivered to the engine; the answers
    // cached here are forgotten after each batch, so a grant or a revocation shows at once
    if let Some(url) = env("PG_AUTH_DATABASE_URL") {
        let answers = state.clone();
        outbox::start(url.clone(), state.engine.clone(), poll, move || {
            answers.lists.clear();
            answers.checks.clear();
        });
        // the engine's rules kept in Postgres (authz.set_rules): new rules may change any answer, plans
        // included
        let answers = state.clone();
        rules::start(url, state.engine.clone(), poll, move || {
            answers.lists.clear();
            answers.checks.clear();
            answers.plans.clear();
        });
    }

    let path = env("PG_AUTH_SOCKET").unwrap_or_else(|| "/tmp/pg_auth.sock".into());
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("bind {path}: {e}"))?;
    // only the owner and its group (the postgres OS user) may connect
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).map_err(|e| e.to_string())?;
    }
    eprintln!("pg-auth-sidecar: listening on {path} ({describe})");

    // SIGTERM (docker stop, systemd, Kubernetes) and SIGINT end it the same way: the socket is
    // removed and the process exits normally
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| format!("SIGTERM handler: {e}"))?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|e| e.to_string())?;
                let state = state.clone();
                tokio::spawn(async move { serve(stream, state).await });
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
        }
    }
    let _ = std::fs::remove_file(&path);
    eprintln!("pg-auth-sidecar: stopped");
    Ok(())
}

async fn serve(stream: UnixStream, state: Arc<State>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut engine = Duration::ZERO;
        let mut asked = None;
        let response = match protocol::parse(&line) {
            Ok(request) => {
                asked = (state.log && request != Request::Ping).then(|| asked_line(&request));
                answer(&state, request, &mut engine).await
            }
            Err(e) => Response::Error { error: e },
        };
        if let Some(asked) = asked {
            eprintln!("{asked} → {}", told_line(&response, state.engine.name(), engine));
        }
        if write.write_all(protocol::encode(&response, engine).as_bytes()).await.is_err() {
            return;
        }
    }
}

/// The response to one request; `engine` accumulates the time spent waiting on the engine.
async fn answer(state: &Arc<State>, request: Request, engine: &mut Duration) -> Response {
    match request {
        Request::Ping => Response::Engine { engine: state.engine.name().into() },
        Request::Fill { subject, permission, object_type, wait_ms } => {
            let Some(index) = &state.index else { return Response::Queued { queued: false, fresh: None } };
            let key = (subject, object_type, permission);
            let queued = index.request(key.clone());
            let fresh = match wait_ms {
                Some(ms) if queued => Some(index.wait(&key, Duration::from_millis(ms)).await),
                _ => None,
            };
            Response::Queued { queued, fresh }
        }
        Request::List { subject, permission, object_type, context } => {
            let key = ((subject, object_type, permission), context_key(&context));
            // the extension keeps an answer for valid_ms: until its half-life, so that it asks
            // again while the answer is still cached here, which refreshes it in the background
            let half = u64::try_from(state.ttl.as_millis()).unwrap_or(u64::MAX) / 2;
            if let Some((cached, left)) = state.lists.get(&key) {
                if left < half {
                    refresh(state, Refresh::List(key));
                }
                return to_list_response(cached, Some(left.saturating_sub(half)));
            }
            // a fresh index set at least this large: too many, without asking the engine (a set
            // holds for no context in particular)
            if context.is_empty() && state.index.as_ref().is_some_and(|i| i.too_many(&key.0, state.list_limit)) {
                return Response::TooMany { too_many: true, valid_ms: None };
            }
            let started = Instant::now();
            let listed = state.engine.list(&key.0, state.list_limit, context.at_now().as_ref()).await;
            *engine += started.elapsed();
            match listed {
                Ok(listed) => {
                    state.lists.put(key, listed.clone());
                    to_list_response(listed, (!state.ttl.is_zero()).then_some(half))
                }
                Err(e) => Response::Error { error: e },
            }
        }
        Request::Plan { subject, permission, object_type, principal } => {
            let key = ((subject, object_type, permission), principal.as_ref().map(ToString::to_string).unwrap_or_default());
            // kept for the TTL, as lists are: the extension keeps it until its half-life
            let half = u64::try_from(state.ttl.as_millis()).unwrap_or(u64::MAX) / 2;
            let (plan, valid_ms) = if let Some((plan, left)) = state.plans.get(&key) {
                if left < half {
                    refresh(state, Refresh::Plan(key));
                }
                (plan, left.saturating_sub(half))
            } else {
                let started = Instant::now();
                let plan = state.engine.plan(&key.0, principal.as_ref()).await;
                *engine += started.elapsed();
                match plan {
                    Ok(plan) => {
                        state.plans.put(key, plan.clone());
                        (plan, half)
                    }
                    Err(e) => return Response::Error { error: e },
                }
            };
            let valid_ms = (!state.ttl.is_zero()).then_some(valid_ms);
            match plan {
                Plan::Always => Response::Plan { plan: "always", condition: None, valid_ms },
                Plan::Never => Response::Plan { plan: "never", condition: None, valid_ms },
                Plan::Rows => Response::Plan { plan: "rows", condition: None, valid_ms },
                Plan::Condition(c) => Response::Plan { plan: "condition", condition: Some(c), valid_ms },
            }
        }
        Request::Check { subject, permission, object_type, ids, attrs, principal, context: asked } => {
            let key = (subject, object_type, permission);
            let request = asked.at_now();
            let context = Context { principal: principal.as_ref(), attrs: attrs.as_deref(), request: request.as_ref() };
            // an answer on attributes, a principal or a context depends on more than the id: not cached
            let caching = !state.ttl.is_zero() && context.principal.is_none() && context.attrs.is_none() && asked.is_empty();
            let mut answers = if caching { state.checks.get(&key, &ids, state.ttl) } else { vec![None; ids.len()] };
            let missing: Vec<String> = ids.iter().zip(&answers).filter(|(_, a)| a.is_none()).map(|(id, _)| id.clone()).collect();
            if !missing.is_empty() {
                let started = Instant::now();
                let checked = state.engine.check(&key, &missing, &context, state.check_concurrency).await;
                *engine += started.elapsed();
                match checked {
                    Ok(results) => {
                        if caching {
                            state.checks.put(&key, missing.iter().zip(results.iter().copied()));
                        }
                        let mut fetched = results.into_iter();
                        for slot in answers.iter_mut().filter(|a| a.is_none()) {
                            *slot = fetched.next();
                        }
                    }
                    Err(e) => return Response::Error { error: e },
                }
            }
            Response::Allowed { allowed: answers.into_iter().map(|a| a.unwrap_or(false)).collect() }
        }
    }
}

/// Ask the engine for a list or plan again in the background and cache the answer, once at a time
/// per answer: the cached one keeps being served (never older than the TTL) until the new one
/// replaces it. An error leaves the old answer to expire.
fn refresh(state: &Arc<State>, what: Refresh) {
    if !state.refreshing.lock().is_ok_and(|mut r| r.insert(what.clone())) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        match &what {
            Refresh::List(key) => {
                let context = serde_json::from_str::<protocol::Asked>(&key.1).unwrap_or_default();
                if let Ok(listed) = state.engine.list(&key.0, state.list_limit, context.at_now().as_ref()).await {
                    state.lists.refreshed(key, listed);
                }
            }
            Refresh::Plan(key) => {
                let principal = serde_json::from_str::<serde_json::Value>(&key.1).ok();
                if let Ok(plan) = state.engine.plan(&key.0, principal.as_ref()).await {
                    state.plans.refreshed(key, plan);
                }
            }
        }
        if let Ok(mut r) = state.refreshing.lock() {
            r.remove(&what);
        }
    });
}

/// Keep the lists and plans used within REFRESH_ACTIVE_MS fresh, whether or not they are asked
/// for: every quarter TTL, refresh those past half their TTL, oldest first, at most
/// REFRESH_CONCURRENCY at a time.
async fn refresher(state: Arc<State>) {
    let mut tick = tokio::time::interval((state.ttl / 4).max(Duration::from_millis(10)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let due = state
            .lists
            .due(state.active)
            .into_iter()
            .map(Refresh::List)
            .chain(state.plans.due(state.active).into_iter().map(Refresh::Plan));
        for what in due {
            if state.refreshing.lock().map_or(true, |r| r.len() >= state.refresh_concurrency) {
                break;
            }
            refresh(&state, what);
        }
    }
}

/// What Postgres asked, for the request log: `list  user:bob document#viewer`.
fn asked_line(request: &Request) -> String {
    match request {
        Request::Ping => "ping".into(),
        Request::List { subject, permission, object_type, context } => {
            format!("list   {subject} {object_type}#{permission}{}", context_shown(context))
        }
        Request::Check { subject, permission, object_type, ids, .. } => {
            format!("check  {subject} {object_type}#{permission} [{}]", ids.join(", "))
        }
        Request::Plan { subject, permission, object_type, .. } => format!("plan   {subject} {object_type}#{permission}"),
        Request::Fill { subject, permission, object_type, .. } => format!("fill   {subject} {object_type}#{permission}"),
    }
}

/// A list's cache key for its context: lists asked with another context are other answers.
fn context_key(context: &protocol::Asked) -> String {
    if context.is_empty() { String::new() } else { serde_json::to_string(context).unwrap_or_default() }
}

/// The context a request gives, for the request log.
fn context_shown(context: &protocol::Asked) -> String {
    let mut parts: Vec<String> = context.context.iter().flatten().map(|(k, v)| format!("{k}={v}")).collect();
    parts.extend(context.now.iter().map(|n| format!("{n}=now")));
    if parts.is_empty() { String::new() } else { format!(" with {}", parts.join(", ")) }
}

/// What Postgres was told, and whether the engine was asked or the cache answered.
fn told_line(response: &Response, engine: &str, waited: Duration) -> String {
    let told = match response {
        Response::Engine { engine } => engine.clone(),
        Response::Ids { ids, .. } => format!("[{}]", ids.join(", ")),
        Response::TooMany { .. } => "too many to list; rows are checked instead".into(),
        Response::Allowed { allowed } => {
            let answers: Vec<&str> = allowed.iter().map(|a| if *a { "yes" } else { "no" }).collect();
            format!("[{}]", answers.join(", "))
        }
        Response::Plan { condition: Some(c), .. } => format!("where {}", filter_text(c)),
        Response::Plan { plan, .. } => (*plan).to_string(),
        Response::Queued { queued, .. } => format!("queued: {queued}"),
        Response::Error { error } => return format!("error: {error}"),
    };
    if waited.is_zero() { format!("{told}  (cached)") } else { format!("{told}  ({engine} {:.1} ms)", waited.as_secs_f64() * 1000.0) }
}

/// A plan's condition as the filter it becomes: `owner = 'bob' or status = 'published'`.
fn filter_text(condition: &serde_json::Value) -> String {
    let op = condition.get("op").and_then(serde_json::Value::as_str);
    let args: Vec<&serde_json::Value> =
        condition.get("args").and_then(serde_json::Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let term = |v: &serde_json::Value| {
        if let Some(column) = v.get("column").and_then(serde_json::Value::as_str) {
            column.to_owned()
        } else if v.get("id").is_some() {
            "id".to_owned()
        } else if let Some(value) = v.get("value") {
            value.as_str().map_or_else(|| value.to_string(), |s| format!("'{s}'"))
        } else {
            format!("({})", filter_text(v))
        }
    };
    match (op, args.as_slice()) {
        (Some(join @ ("and" | "or")), parts) if !parts.is_empty() => parts
            .iter()
            .map(|p| {
                if p.get("op").and_then(serde_json::Value::as_str).is_some_and(|o| matches!(o, "and" | "or") && o != join) {
                    format!("({})", filter_text(p))
                } else {
                    filter_text(p)
                }
            })
            .collect::<Vec<_>>()
            .join(&format!(" {join} ")),
        (Some("not"), [inner]) => format!("not ({})", filter_text(inner)),
        (Some(cmp), [left, right]) => {
            let sign = match cmp {
                "eq" => "=",
                "ne" => "<>",
                "lt" => "<",
                "le" => "<=",
                "gt" => ">",
                "ge" => ">=",
                "in" => "in",
                _ => return condition.to_string(),
            };
            format!("{} {sign} {}", term(left), term(right))
        }
        _ => condition.to_string(),
    }
}

fn to_list_response(listed: Listed, valid_ms: Option<u64>) -> Response {
    match listed {
        Listed::Ids(ids) => Response::Ids { ids, valid_ms },
        Listed::TooMany => Response::TooMany { too_many: true, valid_ms },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_log_names_the_question_the_answer_and_who_gave_it() {
        let list = protocol::Request::List {
            subject: "user:bob".into(),
            permission: "viewer".into(),
            object_type: "document".into(),
            context: protocol::Asked::default(),
        };
        let ids = Response::Ids { ids: vec!["1".into(), "2".into()], valid_ms: None };
        assert_eq!(
            format!("{} → {}", asked_line(&list), told_line(&ids, "openfga", Duration::from_millis(3))),
            "list   user:bob document#viewer → [1, 2]  (openfga 3.0 ms)"
        );
        let allowed = Response::Allowed { allowed: vec![true, false] };
        assert_eq!(told_line(&allowed, "spicedb", Duration::ZERO), "[yes, no]  (cached)");
        assert_eq!(told_line(&Response::Error { error: "down".into() }, "cerbos", Duration::ZERO), "error: down");
    }

    #[test]
    fn request_log_shows_a_plan_as_the_filter_it_becomes() {
        let plan = serde_json::json!({"op": "and", "args": [
            {"op": "or", "args": [
                {"op": "eq", "args": [{"column": "owner"}, {"value": "bob"}]},
                {"op": "eq", "args": [{"column": "status"}, {"value": "published"}]}]},
            {"op": "not", "args": [{"op": "ge", "args": [{"column": "size"}, {"value": 10}]}]}]});
        assert_eq!(filter_text(&plan), "(owner = 'bob' or status = 'published') and not (size >= 10)");
    }

    #[test]
    fn active_answers_are_due_for_refresh_and_idle_ones_expire() {
        let cache: Cache<&str, u32> = Cache::new(Duration::from_millis(40));
        cache.put("used", 1);
        cache.put("idle", 1);
        std::thread::sleep(Duration::from_millis(25));
        // past half the TTL: due while used within the active window
        assert_eq!(cache.get(&"used").map(|(v, _)| v), Some(1));
        let mut due = cache.due(Duration::from_millis(15));
        due.sort_unstable();
        assert_eq!(due, ["used"]);
        // a refresh replaces the answer and is not a use
        cache.refreshed(&"used", 2);
        assert_eq!(cache.due(Duration::from_millis(15)), [] as [&str; 0]);
        std::thread::sleep(Duration::from_millis(25));
        // "idle" expired unused: dropped; "used" is fresh again, no longer active
        assert_eq!(cache.due(Duration::from_millis(15)), [] as [&str; 0]);
        assert!(cache.get(&"idle").is_none());
        assert_eq!(cache.get(&"used").map(|(v, _)| v), Some(2));
        // a refresh of an answer no longer cached is dropped
        cache.refreshed(&"gone", 3);
        assert!(cache.get(&"gone").is_none());
    }
}

#[cfg(test)]
mod serving {
    //! The request loop as the extension uses it: JSON lines over a Unix socket, against an engine
    //! mocked over HTTP.
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use wiremock::matchers::{body_partial_json, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn state(engine: Engine, ttl_ms: u64, active_ms: u64) -> Arc<State> {
        let ttl = Duration::from_millis(ttl_ms);
        Arc::new(State {
            engine: Arc::new(engine),
            index: None,
            list_limit: 100,
            check_concurrency: 4,
            ttl,
            active: Duration::from_millis(active_ms),
            refresh_concurrency: 4,
            lists: Cache::new(ttl),
            plans: Cache::new(ttl),
            refreshing: Mutex::new(HashSet::new()),
            checks: Checks::default(),
            log: false,
        })
    }

    fn openfga(server: &MockServer) -> Engine {
        Engine::OpenFga(OpenFga::new(&server.uri(), "s1", None, None, None, Duration::from_secs(5), Duration::from_secs(3)).unwrap())
    }

    fn cerbos(server: &MockServer) -> Engine {
        Engine::Cerbos(Cerbos::new(&server.uri(), Duration::from_secs(5), None).unwrap())
    }

    /// A connection to a sidecar serving `state`: send a request, read its answer.
    struct Client {
        lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
        write: tokio::net::unix::OwnedWriteHalf,
    }

    impl Client {
        fn connect(state: &Arc<State>) -> Self {
            let (ours, theirs) = UnixStream::pair().unwrap();
            tokio::spawn(serve(theirs, state.clone()));
            let (read, write) = ours.into_split();
            Client { lines: BufReader::new(read).lines(), write }
        }

        async fn ask(&mut self, request: &str) -> Value {
            self.write.write_all(format!("{request}\n").as_bytes()).await.unwrap();
            serde_json::from_str(&self.lines.next_line().await.unwrap().unwrap()).unwrap()
        }
    }

    async fn requests(server: &MockServer, at: &str) -> Vec<Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with(at))
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    const LIST: &str = r#"{"op":"list","subject":"user:anne","permission":"viewer","type":"doc"}"#;

    #[tokio::test]
    async fn lists_are_answered_from_the_cache_until_half_their_ttl() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": ["doc:1", "doc:2"]})))
            .expect(1)
            .mount(&server)
            .await;
        let mut c = Client::connect(&state(openfga(&server), 1000, 0));
        let first = c.ask(LIST).await;
        assert_eq!(first["ids"], json!(["1", "2"]));
        assert_eq!(first["valid_ms"], 500, "the extension keeps it until the half-life");
        let second = c.ask(LIST).await;
        assert_eq!(second["ids"], json!(["1", "2"]));
        assert_eq!(second["engine_us"], 0, "from the cache");
        assert!(second["valid_ms"].as_u64().unwrap() <= 500);
    }

    // another context is another answer; the time asked is not part of it
    #[tokio::test]
    async fn lists_are_kept_per_context() {
        let server = MockServer::start().await;
        for (region, objects) in [("eu", json!(["doc:1"])), ("us", json!(["doc:2"]))] {
            Mock::given(path("/stores/s1/list-objects"))
                .and(body_partial_json(json!({"context": {"region": region}})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": objects})))
                .expect(1)
                .mount(&server)
                .await;
        }
        let mut c = Client::connect(&state(openfga(&server), 1000, 0));
        let list = |region: &str| {
            format!(
                r#"{{"op":"list","subject":"user:anne","permission":"viewer","type":"doc","context":{{"region":"{region}"}},"now":"current_time"}}"#
            )
        };
        assert_eq!(c.ask(&list("eu")).await["ids"], json!(["1"]));
        assert_eq!(c.ask(&list("us")).await["ids"], json!(["2"]));
        let again = c.ask(&list("eu")).await;
        assert_eq!((again["ids"].clone(), again["engine_us"].clone()), (json!(["1"]), json!(0)), "the same context: from the cache");
    }

    #[tokio::test]
    async fn without_a_ttl_nothing_is_cached_or_kept() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": ["doc:1"]})))
            .expect(2)
            .mount(&server)
            .await;
        let mut c = Client::connect(&state(openfga(&server), 0, 0));
        assert!(c.ask(LIST).await.get("valid_ms").is_none());
        c.ask(LIST).await;
    }

    #[tokio::test]
    async fn a_list_used_past_half_its_ttl_is_refreshed_in_the_background() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": ["doc:1"]})))
            .mount(&server)
            .await;
        let mut c = Client::connect(&state(openfga(&server), 200, 0));
        c.ask(LIST).await;
        tokio::time::sleep(Duration::from_millis(120)).await;
        let cached = c.ask(LIST).await;
        assert_eq!(
            (cached["engine_us"].as_u64(), cached["valid_ms"].as_u64()),
            (Some(0), Some(0)),
            "past half: still served, valid no longer"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(requests(&server, "list-objects").await.len(), 2, "refreshed ahead of its expiry");
    }

    #[tokio::test]
    async fn active_lists_stay_fresh_with_nobody_asking() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": ["doc:1"]})))
            .mount(&server)
            .await;
        let s = state(openfga(&server), 200, 10_000);
        tokio::spawn(refresher(s.clone()));
        let mut c = Client::connect(&s);
        c.ask(LIST).await;
        // idle for three TTLs: the refresher kept it fresh
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(requests(&server, "list-objects").await.len() >= 3);
        assert_eq!(c.ask(LIST).await["engine_us"], 0, "no wait on the engine after the pause");
    }

    #[tokio::test]
    async fn checks_ask_only_for_ids_not_cached() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/batch-check"))
            .respond_with(|req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let result: serde_json::Map<String, Value> = body["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| (c["correlation_id"].as_str().unwrap().to_string(), json!({"allowed": c["tuple_key"]["object"] != "doc:2"})))
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({"result": result}))
            })
            .mount(&server)
            .await;
        let mut c = Client::connect(&state(openfga(&server), 1000, 0));
        let check = |ids: &str| format!(r#"{{"op":"check","subject":"user:anne","permission":"viewer","type":"doc","ids":{ids}}}"#);
        assert_eq!(c.ask(&check(r#"["1","2"]"#)).await["allowed"], json!([true, false]));
        assert_eq!(c.ask(&check(r#"["2","3"]"#)).await["allowed"], json!([false, true]));
        let asked: Vec<usize> = requests(&server, "batch-check").await.iter().map(|b| b["checks"].as_array().unwrap().len()).collect();
        assert_eq!(asked, [2, 1], "2 came from the cache");
    }

    #[tokio::test]
    async fn checks_on_attributes_and_plans_follow_the_principal() {
        let server = MockServer::start().await;
        Mock::given(path("/api/check/resources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
                {"resource": {"id": "1"}, "actions": {"read": "EFFECT_ALLOW"}}]})))
            .mount(&server)
            .await;
        for role in ["user", "admin"] {
            Mock::given(path("/api/plan/resources"))
                .and(body_partial_json(json!({"principal": {"roles": [role]}})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"filter": {"kind":
                    if role == "admin" { "KIND_ALWAYS_ALLOWED" } else { "KIND_ALWAYS_DENIED" }}})))
                .mount(&server)
                .await;
        }
        let mut c = Client::connect(&state(cerbos(&server), 1000, 0));
        let check = r#"{"op":"check","subject":"user:anne","permission":"read","type":"note","ids":["1"],"attrs":[{"owner":"anne"}],"principal":{"roles":["user"]}}"#;
        assert_eq!(c.ask(check).await["allowed"], json!([true]));
        c.ask(check).await;
        assert_eq!(requests(&server, "check/resources").await.len(), 2, "an answer on attributes is not cached");
        let plan = |role: &str| {
            format!(r#"{{"op":"plan","subject":"user:anne","permission":"read","type":"note","principal":{{"roles":["{role}"]}}}}"#)
        };
        assert_eq!(c.ask(&plan("user")).await["plan"], "never");
        assert_eq!(c.ask(&plan("admin")).await["plan"], "always", "another principal, another plan");
        assert_eq!(c.ask(&plan("user")).await["engine_us"], 0, "the same principal's plan, cached");
        assert_eq!(requests(&server, "plan/resources").await.len(), 2);
    }

    #[tokio::test]
    async fn malformed_requests_and_engine_errors_are_answered_as_errors() {
        let server = MockServer::start().await;
        Mock::given(path("/stores/s1/list-objects"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({"message": "datastore down"})))
            .mount(&server)
            .await;
        let mut c = Client::connect(&state(openfga(&server), 1000, 0));
        assert!(c.ask("not json").await["error"].is_string());
        assert!(c.ask(r#"{"op":"list"}"#).await["error"].is_string(), "fields missing");
        assert_eq!(c.ask(LIST).await["error"], "openfga /list-objects 500 Internal Server Error: datastore down");
        assert_eq!(c.ask(r#"{"op":"ping"}"#).await["engine"], "openfga", "the connection still serves");
        // no permission index: a fill is not queued
        assert_eq!(c.ask(r#"{"op":"fill","subject":"user:anne","permission":"viewer","type":"doc"}"#).await["queued"], false);
        assert!(c.ask(r#"{"op":"plan","subject":"user:anne","permission":"viewer","type":"doc"}"#).await["error"].is_string());
    }
}
