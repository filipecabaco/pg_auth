//! Who is asking. `authz.subject_type()` / `authz.subject_id()` resolve the subject from exactly
//! one configured source (see `authz.configure`), and `authz.current_subject()` explains the
//! result. NULL subject means no access.

use pgrx::prelude::*;
use pgrx::{GucContext, GucFlags, GucRegistry, GucSetting};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;

pub static SUBJECT_ID: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);
pub static SUBJECT_TYPE: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);
pub static SUBJECT_ROLES: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);
pub static AS_SUBJECT: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);

pub fn init() {
    crate::ffi::watch_config();
    // Superuser-only, so ordinary roles cannot pick their own subject. The API's connection role
    // is allowed with: GRANT SET ON PARAMETER pg_auth.subject_id, pg_auth.subject_type TO <role>.
    GucRegistry::define_string_guc(
        c"pg_auth.subject_id",
        c"Subject id of the current request (authz.configure(subject => 'guc')).",
        c"Only superusers and roles granted SET ON PARAMETER pg_auth.subject_id can change it.",
        &SUBJECT_ID,
        GucContext::Suset,
        GucFlags::default(),
    );
    // An administrator seeing the data as someone else does (authz.why, the SQL editor): it
    // overrides the subject source, so it is superuser-only like pg_auth.subject_id.
    GucRegistry::define_string_guc(
        c"pg_auth.as_subject",
        c"Act as this subject ('type:id', or an id of the default type), whatever the subject source.",
        c"For administrators debugging what a subject sees; only superusers can set it.",
        &AS_SUBJECT,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"pg_auth.subject_roles",
        c"The current request's roles, comma separated, for an engine that decides on them (authz.configure(subject => 'guc')).",
        c"Only superusers and roles granted SET ON PARAMETER pg_auth.subject_roles can change it.",
        &SUBJECT_ROLES,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"pg_auth.subject_type",
        c"Subject type of the current request (authz.configure(subject => 'guc')).",
        c"Defaults to authz.settings subject_type when unset.",
        &SUBJECT_TYPE,
        GucContext::Suset,
        GucFlags::default(),
    );
}

struct Subject {
    source: String,
    kind: String,
    id: Option<String>,
    detail: String,
    /// roles and attributes, for an engine that decides on them (Cerbos)
    principal: Option<Value>,
}

/// A resolved subject (`type:id`) and its principal, with a hash of the principal's content.
#[derive(Clone, Default)]
struct Who {
    subject: Option<String>,
    principal: Option<Value>,
    principal_key: u64,
}

impl Who {
    fn new(subject: Option<String>, principal: Option<Value>) -> Who {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        subject.hash(&mut h);
        principal.as_ref().map(Value::to_string).hash(&mut h);
        Who { subject, principal, principal_key: h.finish() }
    }
}

#[pg_extern(stable, parallel_safe, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn subject_type() -> String {
    resolve().kind
}

#[pg_extern(stable, parallel_safe, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn subject_id() -> Option<String> {
    resolve().id
}

/// The subject pg_auth sees for this request, where it came from, and why it is missing if so.
#[pg_extern(stable, security_definer)]
#[search_path(pg_catalog, pg_temp)]
fn current_subject()
-> TableIterator<'static, (name!(source, String), name!(subject_type, String), name!(subject_id, Option<String>), name!(detail, String))> {
    let s = resolve();
    TableIterator::once((s.source, s.kind, s.id, s.detail))
}

/// authz.settings, shared between the callers of one configuration generation.
type Settings = std::rc::Rc<HashMap<String, String>>;

thread_local! {
    /// authz.settings as of a configuration generation (ffi::config_generation): kept across
    /// transactions until authz.settings changes.
    static SETTINGS: RefCell<Option<(u64, Settings)>> = const { RefCell::new(None) };
}

const SETTINGS_QUERY: &str = "SELECT coalesce(jsonb_object_agg(key, value), '{}')::text FROM authz.settings";
const SUBJECT_QUERY: &str = "SELECT authz.subject_type() || ':' || authz.subject_id()";

/// The queries subject resolution runs, for a warm-up to prepare (authz.warm).
pub fn queries() -> Vec<(&'static str, Vec<pg_sys::Oid>)> {
    vec![(SETTINGS_QUERY, vec![]), (SUBJECT_QUERY, vec![])]
}

/// Load authz.settings into this backend (authz.warm): how many there are.
pub fn warm() -> usize {
    settings().len()
}

fn settings() -> Settings {
    if let Some(cached) = cached_settings() {
        return cached;
    }
    let generation = crate::ffi::config_generation();
    let loaded: HashMap<String, String> =
        crate::ffi::read_latest::<String>(SETTINGS_QUERY).and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default();
    let loaded = std::rc::Rc::new(loaded);
    SETTINGS.with(|s| *s.borrow_mut() = Some((generation, loaded.clone())));
    loaded
}

/// authz.settings, when this backend holds them for the current configuration generation.
fn cached_settings() -> Option<Settings> {
    let generation = crate::ffi::config_generation();
    SETTINGS.with(|s| s.borrow().as_ref().filter(|(g, _)| *g == generation).map(|(_, m)| m.clone()))
}

/// Keep a resolved subject: across transactions while the configuration and the request
/// settings it was read from hold.
fn keep(cell: &'static std::thread::LocalKey<RefCell<Option<Resolved>>>, inputs: Inputs, who: Who) {
    cell.with(|c| *c.borrow_mut() = Some((crate::ffi::config_generation(), inputs, who)));
}

/// A kept subject, if it still holds.
fn kept(cell: &'static std::thread::LocalKey<RefCell<Option<Resolved>>>) -> Option<Who> {
    let generation = crate::ffi::config_generation();
    cell.with(|c| c.borrow().as_ref().filter(|(g, inputs, _)| *g == generation && inputs.hold()).map(|(_, _, w)| w.clone()))
}

/// A subject, the configuration generation and the inputs it was resolved from.
type Resolved = (u64, Inputs, Who);

thread_local! {
    /// What the last resolve() read besides the settings: each setting's name and value, and
    /// until when a verified token holds. None when its answer must not be reused (a rejected
    /// token may become valid).
    static INPUTS: RefCell<Option<Inputs>> = const { RefCell::new(None) };
    /// The subject current_as_caller resolved last, and from what.
    static LAST: RefCell<Option<Resolved>> = const { RefCell::new(None) };
    /// The same for current() (security definer code, which resolves directly).
    static CURRENT: RefCell<Option<Resolved>> = const { RefCell::new(None) };
}

#[derive(Clone, Default)]
struct Inputs {
    settings: Vec<(std::ffi::CString, Option<String>)>,
    until: Option<i64>,
}

impl Inputs {
    /// Whether resolving again now would read the same values (with the same configuration).
    fn hold(&self) -> bool {
        self.settings.iter().all(|(name, value)| crate::ffi::config_option_is(name, value.as_deref()))
            && self.until.is_none_or(|until| now() <= until)
    }
}

fn read(inputs: &mut Inputs, name: &str) -> Option<String> {
    let value = setting(name);
    inputs.settings.push((std::ffi::CString::new(name).unwrap_or_default(), value.clone()));
    value
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Statement triggers on authz.settings and authz.hierarchies call this: every backend reloads
/// pg_auth's configuration once the change commits.
#[pg_extern(volatile)]
fn invalidate_config() {
    crate::ffi::invalidate_config();
}

fn setting(name: &str) -> Option<String> {
    crate::ffi::config_option(name)
}

/// The current subject as `type:id`, or None when there is none (nothing is allowed).
/// Reused while the request settings it was resolved from are unchanged: authz.permits asks
/// once per row of a write.
pub fn current() -> Option<String> {
    who().subject
}

/// The current subject's principal: its roles and attributes, for an engine that decides on
/// them. None without a subject.
pub fn principal() -> Option<Value> {
    who().principal
}

/// Whether the subject was last resolved from setting `name` (the request settings it read).
pub fn reads(name: &std::ffi::CStr) -> bool {
    let reads = |cell: &'static std::thread::LocalKey<RefCell<Option<Resolved>>>| {
        cell.with(|c| c.borrow().as_ref().is_some_and(|(_, inputs, _)| inputs.settings.iter().any(|(n, _)| n.as_c_str() == name)))
    };
    reads(&LAST) || reads(&CURRENT)
}

/// The current subject and principal, as a hash of their content: what was derived from them (a
/// plan) holds while it is the same, across transactions and resolutions (a request's subject
/// comes and goes with its transaction-local settings).
pub fn principal_key() -> u64 {
    who().principal_key
}

/// Whether the current subject is resolved and still holds (no request setting it came from has
/// changed): then `current`, `principal` and `generation` answer from memory, without reading
/// authz.settings, so code running as the caller (the planner hook) may use them.
pub fn resolved() -> bool {
    kept(&CURRENT).is_some()
}

fn who() -> Who {
    if let Some(who) = kept(&CURRENT) {
        return who;
    }
    let s = resolve();
    let who = Who::new(s.id.map(|id| format!("{}:{id}", s.kind)), s.principal);
    if let Some(inputs) = INPUTS.with(|i| i.borrow_mut().take()) {
        keep(&CURRENT, inputs, who.clone());
    }
    who
}

/// Same, for code running as the querying role (the planner hook): goes through the security
/// definer functions, so the role never reads authz.settings (which may hold signing secrets).
///
/// That query is the planner's main fixed cost (~40 us), paid again by every scan, so the answer
/// is reused, across transactions too, while what the functions read is unchanged: the
/// configuration (`ffi::config_generation`) and the request settings the security definer code
/// recorded as its inputs (`resolve`), which the caller set and may read.
pub fn current_as_caller() -> Option<String> {
    if let Some(who) = kept(&LAST) {
        return who.subject;
    }
    // with the configuration in memory, resolving reads only the request's settings, which the
    // caller set and may read: no query
    if cached_settings().is_some() {
        return who().subject;
    }
    INPUTS.with(|i| *i.borrow_mut() = None);
    let subject = crate::ffi::read_one::<String>(SUBJECT_QUERY, &[]);
    if let Some(inputs) = INPUTS.with(|i| i.borrow_mut().take()) {
        keep(&LAST, inputs, Who::new(subject.clone(), None));
    }
    subject
}

fn resolve() -> Subject {
    let mut inputs = Inputs::default();
    let subject = resolve_from(&mut inputs);
    // a rejected token is not reused: it may become valid (nbf) within the transaction
    let reusable = get_source() != "jwt_signed" || inputs.until.is_some();
    INPUTS.with(|i| *i.borrow_mut() = reusable.then_some(inputs));
    subject
}

fn get_source() -> String {
    settings().get("subject_source").cloned().unwrap_or_default()
}

fn resolve_from(inputs: &mut Inputs) -> Subject {
    let cfg = settings();
    let get = |k: &str| cfg.get(k).cloned().unwrap_or_default();
    let source = get("subject_source");
    let default_type = get("subject_type");

    let roles_claim = get("roles_claim");
    let found = |subject_type: String, id: Option<String>, detail: String, principal: Option<Value>| Subject {
        source: source.clone(),
        kind: subject_type,
        principal: id.as_ref().and(principal),
        id,
        detail,
    };

    // an administrator acting as someone: whatever the configured source
    if let Some(acting) = read(inputs, "pg_auth.as_subject").filter(|s| !s.is_empty()) {
        let (ty, id) = acting.split_once(':').map_or((default_type, acting.clone()), |(t, i)| (t.to_owned(), i.to_owned()));
        let roles: Vec<String> = read(inputs, "pg_auth.subject_roles")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_owned)
            .collect();
        return Subject {
            source: "as_subject".into(),
            kind: ty,
            id: Some(id),
            detail: "pg_auth.as_subject: an administrator acting as this subject".into(),
            principal: Some(serde_json::json!({"roles": roles, "attr": {}})),
        };
    }
    match source.as_str() {
        "guc" => {
            let id = read(inputs, "pg_auth.subject_id").filter(|s| !s.is_empty());
            let ty = read(inputs, "pg_auth.subject_type").filter(|s| !s.is_empty()).unwrap_or(default_type);
            let detail = if id.is_some() { "pg_auth.subject_id" } else { "pg_auth.subject_id is not set" };
            let roles: Vec<String> = read(inputs, "pg_auth.subject_roles")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_owned)
                .collect();
            found(ty, id, detail.into(), Some(serde_json::json!({"roles": roles, "attr": {}})))
        }
        "jwt" => {
            let claims_setting = get("claims_setting");
            match read(inputs, &claims_setting).and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                None => found(default_type, None, format!("{claims_setting} is not set or not JSON"), None),
                Some(claims) => {
                    let id = claim(&claims, &get("claim"));
                    let detail = match &id {
                        Some(_) => format!("claim {} of {claims_setting} (not signature-checked)", get("claim")),
                        None => format!("claim {} missing from {claims_setting}", get("claim")),
                    };
                    found(default_type, id, detail, Some(principal_of(&claims, &roles_claim)))
                }
            }
        }
        "jwt_signed" => match bearer_token(read(inputs, &get("token_setting")), &get("token_setting"), &get("token_header")) {
            Err(why) => found(default_type, None, why, None),
            Ok(token) => {
                let skew = get("clock_skew_seconds").parse().unwrap_or(30);
                match verified_claims(&get("version"), &get("jwks"), &token, skew) {
                    Err(why) => found(default_type, None, format!("token rejected: {why}"), None),
                    Ok(claims) => {
                        // holds until it expires (verify requires exp)
                        inputs.until = claims.get("exp").and_then(Value::as_i64).map(|exp| exp + skew);
                        let id = claim(&claims, &get("claim"));
                        let detail = match &id {
                            Some(_) => format!("claim {} of a verified token", get("claim")),
                            None => format!("verified token has no claim {}", get("claim")),
                        };
                        found(default_type, id, detail, Some(principal_of(&claims, &roles_claim)))
                    }
                }
            }
        },
        other => found(default_type, None, format!("unknown subject source {other:?}"), None),
    }
}

/// A principal from a token's claims: its roles from the claim at `roles_claim` (a string or an
/// array of strings), its attributes the claims.
fn principal_of(claims: &Value, roles_claim: &str) -> Value {
    let roles: Vec<String> = match roles_claim.split('.').try_fold(claims, |v, key| v.get(key)) {
        Some(Value::String(r)) if !r.is_empty() => vec![r.clone()],
        Some(Value::Array(rs)) => rs.iter().filter_map(Value::as_str).filter(|r| !r.is_empty()).map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    serde_json::json!({"roles": roles, "attr": claims})
}

/// `sub`, or a dotted path such as `app_metadata.user_id`. Numbers are accepted as ids.
fn claim(claims: &Value, path: &str) -> Option<String> {
    let v = path.split('.').try_fold(claims, |v, key| v.get(key))?;
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The token from `Authorization: Bearer ...` in a JSON headers setting (PostgREST's
/// request.headers), or the whole value when the setting holds a bare token.
fn bearer_token(raw: Option<String>, token_setting: &str, header: &str) -> Result<String, String> {
    let raw = raw.ok_or_else(|| format!("{token_setting} is not set"))?;
    let value = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(headers)) => headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(header))
            .and_then(|(_, v)| v.as_str().map(str::to_owned))
            .ok_or_else(|| format!("no {header} header in {token_setting}"))?,
        _ => raw,
    };
    let token = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")).unwrap_or(&value).trim();
    if token.is_empty() {
        return Err("empty bearer token".into());
    }
    Ok(token.to_owned())
}

thread_local! {
    /// Verified claims per (settings version, token). Expiry is re-checked on every hit.
    static VERIFIED: RefCell<HashMap<(String, String), Value>> = RefCell::new(HashMap::new());
}

fn verified_claims(version: &str, jwks: &str, token: &str, skew: i64) -> Result<Value, String> {
    let now = now();
    let key = (version.to_owned(), token.to_owned());

    if let Some(claims) = VERIFIED.with(|c| c.borrow().get(&key).cloned()) {
        return match claims.get("exp").and_then(Value::as_i64) {
            Some(exp) if now <= exp + skew => Ok(claims),
            _ => Err("token expired".into()),
        };
    }

    let jwks: Value = serde_json::from_str(jwks).map_err(|_| "jwks is not configured".to_string())?;
    let claims = crate::jwt::verify(token, &jwks, now, skew)?;
    VERIFIED.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.len() > 1024 {
            cache.clear();
        }
        cache.insert(key, claims.clone());
    });
    Ok(claims)
}
