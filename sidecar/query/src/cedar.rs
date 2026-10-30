//! Cedar evaluated in the sidecar itself: no engine server. Its policies and schema are kept in
//! Postgres (authz.set_rules: a rule named `schema` is the schema, every other one policies) and
//! loaded by every sidecar (rules.rs); entities' parents (`User in Group`, `Document in Folder`)
//! are the relationship tables' rows whose relation is `in`, the object's parent being the
//! subject, mirrored here from Postgres (`relationships_now`) and kept current with the deliveries.
//!
//! `plan` asks Cedar's typed partial evaluation which resources of a type the principal may act on,
//! the resource unknown: the policies left over are conditions on the resource, translated into the
//! protocol's language (protocol.rs). An attribute is a column (`resource.status`), `resource in
//! Folder::"f1"` the ids below that folder, `resource has a` a column not null; a part outside the
//! language asks Cedar about the row (`check`). `check` evaluates the request with the row's
//! attributes and the entities' ancestors; an entity's own attributes other than the principal's
//! and the resource's are unknown to it, and a policy that needs them does not apply.
//!
//! A null column is an absent attribute. Cedar's validator requires `has` before an optional
//! attribute is read; a required one absent makes the row invalid to Cedar, which denies it, so a
//! condition holds only for rows where every required attribute is present.

use crate::cerbos::Plan;
use crate::engine::{CallError, Change, Listed, Rule};
use cedar_policy::{
    Context, EntityTypeName, EntityUid, PartialEntities, PartialEntityUid, PartialRequest, PolicyId, PolicySet, Schema, SchemaFragment,
    ValidationMode, Validator,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// An entity: its type and id.
type Uid = (String, String);

#[derive(Default)]
pub struct Cedar {
    rules: RwLock<Option<Arc<Loaded>>>,
    store: RwLock<Store>,
    /// bumped by every change to the store, so a reload that raced a change is made again
    version: AtomicU64,
    /// woken when another sidecar delivered changes: the store is read again (`reload`)
    pub stale: tokio::sync::Notify,
    /// woken when the parents or the rules changed: the permission index's sets are stale
    /// (`follow`); `changes` counts them, so a wake-up left from before a feed began reports nothing
    changed: tokio::sync::Notify,
    changes: AtomicU64,
}

/// The rules in force, compiled.
struct Loaded {
    named: BTreeMap<String, String>,
    id: String,
    policies: PolicySet,
    schema: Schema,
    types: HashMap<String, Shape>,
}

/// An entity type as the schema declares it: its attributes (kind, required) and the types it
/// may be `in`.
#[derive(Default)]
struct Shape {
    attributes: BTreeMap<String, (Kind, bool)>,
    member_of: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    String,
    Long,
    Bool,
    Entity(String),
    Set(Box<Kind>),
    /// its attributes: kind, required
    Record(BTreeMap<String, (Kind, bool)>),
    Other,
}

#[derive(Default, PartialEq)]
struct Store {
    parents: HashMap<Uid, HashSet<Uid>>,
    children: HashMap<Uid, HashSet<Uid>>,
}

impl Store {
    fn add(&mut self, child: Uid, parent: Uid) {
        self.children.entry(parent.clone()).or_default().insert(child.clone());
        self.parents.entry(child).or_default().insert(parent);
    }

    fn remove(&mut self, child: &Uid, parent: &Uid) {
        if let Some(p) = self.parents.get_mut(child) {
            p.remove(parent);
        }
        if let Some(c) = self.children.get_mut(parent) {
            c.remove(child);
        }
    }

    /// Every entity reached from `from` along `edges` the schema allows (`allowed(from-side,
    /// to-side)`), `from` itself left out: a row relating types the schema does not is ignored.
    fn reach(edges: &HashMap<Uid, HashSet<Uid>>, from: &Uid, allowed: impl Fn(&Uid, &Uid) -> bool) -> HashSet<Uid> {
        let mut seen = HashSet::new();
        let mut queue: VecDeque<Uid> = VecDeque::from([from.clone()]);
        while let Some(at) = queue.pop_front() {
            for next in edges.get(&at).into_iter().flatten() {
                if next != from && allowed(&at, next) && seen.insert(next.clone()) {
                    queue.push_back(next.clone());
                }
            }
        }
        seen
    }
}

const SCHEMA: &str = "schema";

/// Ids checked per call (one evaluation each, in this process).
pub const MAX_BATCH: usize = 100;

/// A rule is the schema when it is named `schema` (or `<something>.cedarschema`).
pub fn is_schema(name: &str) -> bool {
    name == SCHEMA || name.ends_with(".cedarschema")
}

/// `type:id` (the extension's subjects, the relationships' objects and subjects) as an entity.
fn uid_of(reference: &str) -> Result<Uid, String> {
    match reference.split_once(':') {
        Some((ty, id)) if !ty.is_empty() && !id.is_empty() && !ty.contains('#') => Ok((ty.to_owned(), id.to_owned())),
        _ => Err(format!("`{reference}` is not <type>:<id>")),
    }
}

fn entity_json((ty, id): &Uid) -> Value {
    json!({"type": ty, "id": id})
}

impl Cedar {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- rules

    /// The rules in force, as rules.rs reads them (`authz_sidecar.rules_in_force`): loaded
    /// unless they are the ones already loaded. Whether they changed.
    pub fn load(&self, named: BTreeMap<String, String>) -> Result<bool, String> {
        if self.rules.read().map_err(|_| "cedar: rules lock")?.as_ref().is_some_and(|l| l.named == named) {
            return Ok(false);
        }
        let loaded = compile(named)?;
        *self.rules.write().map_err(|_| "cedar: rules lock")? = Some(Arc::new(loaded));
        self.changes.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_one();
        Ok(true)
    }

    /// New versions of some rules: compiled with the rest of those in force, and taken when the
    /// whole compiles and validates against the schema. The rules' id (a hash of all of them).
    pub fn push(&self, rules: &[Rule]) -> Result<String, CallError> {
        let mut named = self.rules.read().ok().and_then(|r| r.as_ref().map(|l| l.named.clone())).unwrap_or_default();
        for r in rules {
            named.insert(r.name.clone(), r.body.clone());
        }
        let loaded = compile(named).map_err(|message| CallError { permanent: true, message })?;
        let id = loaded.id.clone();
        *self.rules.write().map_err(|_| CallError { permanent: false, message: "cedar: rules lock".into() })? = Some(Arc::new(loaded));
        self.changes.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_one();
        Ok(id)
    }

    fn loaded(&self) -> Result<Arc<Loaded>, String> {
        self.rules
            .read()
            .ok()
            .and_then(|r| r.clone())
            .ok_or_else(|| "cedar: no schema and policies stored (authz.set_rules('cedar', ...))".into())
    }

    // ---- entities' parents

    /// Relationship changes the outbox delivers, each an `in` (the object's parent is the subject),
    /// read: an error for one Cedar cannot take. They are applied (`apply`) once the delivery that
    /// keeps them in Postgres has committed, so no read of Postgres meanwhile undoes them.
    pub fn validate(changes: &[Change]) -> Result<Vec<(bool, Uid, Uid)>, CallError> {
        let permanent = |message: String| CallError { permanent: true, message };
        changes
            .iter()
            .map(|c| {
                if c.relation != "in" {
                    return Err(permanent(format!("cedar: a relationship is `in` (the object in the subject), not `{}`", c.relation)));
                }
                if c.condition.is_some() {
                    return Err(permanent("cedar: an `in` takes no condition".into()));
                }
                Ok((c.write, uid_of(&c.object).map_err(permanent)?, uid_of(&c.subject).map_err(permanent)?))
            })
            .collect()
    }

    /// Delivered changes, committed: into this sidecar's parents.
    pub fn apply(&self, changes: &[Change]) -> Result<(), CallError> {
        let edges = Self::validate(changes)?;
        let mut store = self.store.write().map_err(|_| CallError { permanent: false, message: "cedar: store lock".into() })?;
        for (write, child, parent) in edges {
            if write {
                store.add(child, parent);
            } else {
                store.remove(&child, &parent);
            }
        }
        self.version.fetch_add(1, Ordering::SeqCst);
        self.changes.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_one();
        Ok(())
    }

    /// The store's version, before a reload reads Postgres.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Every `in` relationship as Postgres has it now, read since `since` (`version`): taken
    /// unless a change was applied meanwhile (the caller reads again). Whether it was taken.
    pub fn replace(&self, rows: &[(String, String, String)], since: u64) -> Result<bool, String> {
        let mut store = Store::default();
        for (object, relation, subject) in rows {
            if relation == "in" {
                store.add(uid_of(object)?, uid_of(subject)?);
            }
        }
        let mut current = self.store.write().map_err(|_| "cedar: store lock")?;
        if self.version.load(Ordering::SeqCst) != since {
            return Ok(false);
        }
        // the same parents read again (a periodic read, another sidecar's delivery already
        // applied here): nothing for the index to throw away
        if *current != store {
            *current = store;
            self.version.fetch_add(1, Ordering::SeqCst);
            self.changes.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_one();
        }
        Ok(true)
    }

    // ---- questions

    /// Which resources of `object_type` the subject may `permission`, as a condition on the
    /// resource; `limit`: the most ids a `resource in` becomes, beyond which the row is checked.
    pub fn plan(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        principal: Option<&Value>,
        limit: usize,
    ) -> Result<Plan, String> {
        self.plan_rows(subject, permission, object_type, principal, limit, true)
    }

    /// `plan`; `with_attributes`: the rows come with their attributes, so a row missing a required
    /// one, which Cedar would not take, is allowed by no condition.
    fn plan_rows(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        principal: Option<&Value>,
        limit: usize,
        with_attributes: bool,
    ) -> Result<Plan, String> {
        let loaded = self.loaded()?;
        let store = self.store.read().map_err(|_| "cedar: store lock")?;
        let principal_uid = uid_of(subject)?;
        let principal_ancestors = principal_ancestors(&loaded, &store, &principal_uid, principal);
        let entities = principal_entities(&loaded, &store, &principal_uid, principal, principal_ancestors.as_ref());
        let request = PartialRequest::new(
            PartialEntityUid::from_concrete(euid(&principal_uid)?),
            action(permission)?,
            PartialEntityUid::new(type_name(object_type)?, None),
            Some(Context::empty()),
            &loaded.schema,
        )
        .map_err(|e| format!("cedar: {e}"))?;
        let entities =
            PartialEntities::from_json_value(Value::Array(dedup(entities)), &loaded.schema).map_err(|e| format!("cedar: {e}"))?;
        let response = loaded.policies.tpe(&request, &entities, &loaded.schema).map_err(|e| format!("cedar: {e}"))?;
        // rows given with attributes allow nothing when one Cedar requires is missing
        let required = if with_attributes { required(&loaded, object_type) } else { Vec::new() };
        let present = required
            .into_iter()
            .fold(Cond::True, |all, a| Cond::and(all, Cond::Expr(json!({"op": "ne", "args": [{"column": a}, {"value": null}]}))));
        let outcome = |c: Cond| match c {
            Cond::True => Plan::Always,
            Cond::False => Plan::Never,
            Cond::Check => Plan::Rows,
            Cond::Expr(e) => Plan::Condition(e),
        };
        match response.decision() {
            Some(cedar_policy::Decision::Allow) => return Ok(outcome(present)),
            Some(cedar_policy::Decision::Deny) => return Ok(Plan::Never),
            None => {}
        }
        let tr = Translate {
            resource_type: object_type,
            loaded: &loaded,
            store: &store,
            limit,
            principal: &principal_uid,
            principal_ancestors: principal_ancestors.as_ref(),
        };
        let mut permits = if response.true_permits().next().is_some() { Cond::True } else { Cond::False };
        let mut forbids = Cond::False;
        for policy in response.residual_policies() {
            let body = policy.to_json().ok().and_then(|j| residual_body(&j));
            let translated = body.as_ref().and_then(|b| tr.cond(b));
            if policy.effect() == cedar_policy::Effect::Permit {
                permits = Cond::or(permits, translated.unwrap_or(Cond::Check));
            } else {
                // a forbid outside the language: every row is asked about
                let Some(t) = translated else { return Ok(Plan::Rows) };
                forbids = Cond::or(forbids, t);
            }
        }
        Ok(outcome(Cond::and(present, Cond::and(permits, Cond::not(forbids)))))
    }

    /// The ids of `object_type` the subject may access when its policies name them (`resource ==`,
    /// `resource in`), else TooMany: its rows are checked.
    pub fn list(&self, subject: &str, permission: &str, object_type: &str, limit: usize) -> Result<Listed, String> {
        Ok(match self.plan_rows(subject, permission, object_type, None, limit, false)? {
            Plan::Never => Listed::Ids(Vec::new()),
            Plan::Condition(c) => ids_only(&c).map_or(Listed::TooMany, |ids| Listed::Ids(ids.into_iter().collect())),
            Plan::Always | Plan::Rows => Listed::TooMany,
        })
    }

    /// Every object of `object_type` the subject may access, for the permission index: when its
    /// policies name them by id (`resource in`, `resource ==`), none past a limit; otherwise Cedar
    /// cannot list them (the extension then checks rows).
    pub fn list_all(&self, subject: &str, permission: &str, object_type: &str) -> Result<Vec<String>, String> {
        match self.plan_rows(subject, permission, object_type, None, usize::MAX, false)? {
            Plan::Never => Ok(Vec::new()),
            Plan::Condition(c) => ids_only(&c)
                .map(|ids| ids.into_iter().collect())
                .ok_or_else(|| format!("cedar: {object_type}#{permission}'s policies do not name its objects by id: rows are checked")),
            Plan::Always | Plan::Rows => {
                Err(format!("cedar: {object_type}#{permission}'s policies do not name its objects by id: rows are checked"))
            }
        }
    }

    /// Changes for the permission index: changes may have been missed before this sidecar started
    /// (a gap), then one report per change to the parents or the rules.
    pub async fn follow(&self, feed: tokio::sync::mpsc::UnboundedSender<crate::engine::Feed>) {
        let mut seen = self.changes.load(Ordering::SeqCst);
        if feed.send(crate::engine::Feed::Gap).is_err() {
            return;
        }
        loop {
            self.changed.notified().await;
            let now = self.changes.load(Ordering::SeqCst);
            if now != seen {
                seen = now;
                if feed.send(crate::engine::Feed::Changed(None)).is_err() {
                    return;
                }
            }
        }
    }

    /// One answer per id: the request evaluated with the row's attributes (when given) and the
    /// ancestors of the principal and the resource. A row given without a required attribute is
    /// not a valid entity to Cedar: denied.
    pub fn check(
        &self,
        subject: &str,
        permission: &str,
        object_type: &str,
        ids: &[&String],
        attrs: &[Option<&Value>],
        principal: Option<&Value>,
    ) -> Result<Vec<bool>, String> {
        let loaded = self.loaded()?;
        let store = self.store.read().map_err(|_| "cedar: store lock")?;
        let principal_uid = uid_of(subject)?;
        let principal_ancestors = principal_ancestors(&loaded, &store, &principal_uid, principal);
        let base = principal_entities(&loaded, &store, &principal_uid, principal, principal_ancestors.as_ref());
        let action = action(permission)?;
        let decide = |entities: &PartialEntities, resource: &Uid| -> Result<bool, String> {
            let request = PartialRequest::new(
                PartialEntityUid::from_concrete(euid(&principal_uid)?),
                action.clone(),
                PartialEntityUid::from_concrete(euid(resource)?),
                Some(Context::empty()),
                &loaded.schema,
            )
            .map_err(|e| format!("cedar: {e}"))?;
            let response = loaded.policies.tpe(&request, entities, &loaded.schema).map_err(|e| format!("cedar: {e}"))?;
            Ok(response.decision() == Some(cedar_policy::Decision::Allow))
        };
        // Every row's entity, the principal's and the ancestors in one set, validated once: each
        // row is then one evaluation (building the set per row cost twenty times the evaluation).
        // A row repeating an id with other attributes is evaluated in a set of its own.
        let mut out = vec![false; ids.len()];
        let mut rows: Vec<(usize, Uid)> = Vec::new();
        let mut alone: Vec<(usize, Uid, Value)> = Vec::new();
        let mut entities: Vec<Value> = Vec::new();
        let mut seen: HashMap<String, Value> = HashMap::new();
        for (i, id) in ids.iter().enumerate() {
            let resource = (object_type.to_owned(), (*id).clone());
            let mut entity = Map::new();
            entity.insert("uid".into(), entity_json(&resource));
            if let Some(given) = attrs.get(i).copied().flatten() {
                let Some(a) = attributes(&loaded, object_type, given) else { continue };
                entity.insert("attrs".into(), a);
            }
            entity.insert("parents".into(), Value::Array(ancestors(&loaded, &store, &resource).iter().map(entity_json).collect()));
            let entity = Value::Object(entity);
            match seen.get(*id) {
                Some(first) if *first != entity => alone.push((i, resource, entity)),
                Some(_) => rows.push((i, resource)),
                None => {
                    seen.insert((*id).clone(), entity.clone());
                    entities.push(entity);
                    rows.push((i, resource));
                }
            }
        }
        // the rows first: kept over the same entity among the principal's ancestors (`dedup`),
        // with the attributes only they have
        let with_others = |mut set: Vec<Value>, resources: &mut dyn Iterator<Item = &Uid>| {
            set.extend(base.iter().cloned());
            for r in resources {
                set.extend(ancestors_json(&loaded, &store, r));
            }
            PartialEntities::from_json_value(Value::Array(dedup(set)), &loaded.schema).map_err(|e| format!("cedar: {e}"))
        };
        if !rows.is_empty() {
            let shared = with_others(entities, &mut rows.iter().map(|(_, r)| r))?;
            for (i, resource) in &rows {
                out[*i] = decide(&shared, resource)?;
            }
        }
        for (i, resource, entity) in alone {
            let own = with_others(vec![entity], &mut std::iter::once(&resource))?;
            out[i] = decide(&own, &resource)?;
        }
        Ok(out)
    }
}

// ---- compiling the rules

fn compile(named: BTreeMap<String, String>) -> Result<Loaded, String> {
    let mut schemas = named.iter().filter(|(n, _)| is_schema(n));
    let (schema_name, schema_text) = schemas.next().ok_or("cedar: no schema: store one with authz.set_rules('cedar', 'schema', ...)")?;
    if let Some((other, _)) = schemas.next() {
        return Err(format!("cedar: one schema, not two ({schema_name}, {other})"));
    }
    let (schema, _) = Schema::from_cedarschema_str(schema_text).map_err(|e| format!("cedar schema: {e}"))?;
    let (fragment, _) = SchemaFragment::from_cedarschema_str(schema_text).map_err(|e| format!("cedar schema: {e}"))?;
    let shapes = fragment.to_json_value().map_err(|e| format!("cedar schema: {e}"))?;
    let types = shapes_of(&shapes)?;
    // pg_auth asks with an empty context: an action that requires one would refuse every request
    for (action, decl) in shapes[""]["actions"].as_object().into_iter().flatten() {
        let context = &decl["appliesTo"]["context"];
        let required = |attrs: &Map<String, Value>| attrs.values().any(|a| a.get("required").and_then(Value::as_bool).unwrap_or(true));
        let takes = match context {
            Value::Null => false,
            c if c["type"] == "Record" => c["attributes"].as_object().is_some_and(required),
            _ => true,
        };
        if takes {
            return Err(format!(
                "cedar schema: action {action} requires a context, which pg_auth does not send: make its attributes optional (`?`)"
            ));
        }
    }
    let mut policies = PolicySet::new();
    for (name, text) in named.iter().filter(|(n, _)| !is_schema(n)) {
        let set = PolicySet::from_str(text).map_err(|e| format!("cedar policies {name}: {e}"))?;
        if set.templates().next().is_some() {
            return Err(format!("cedar policies {name}: templates are not supported"));
        }
        for (i, p) in set.policies().enumerate() {
            let id = PolicyId::new(format!("{name}/{i}"));
            policies.add(p.new_id(id)).map_err(|e| format!("cedar policies {name}: {e}"))?;
        }
    }
    let validated = Validator::new(schema.clone()).validate(&policies, ValidationMode::Strict);
    if !validated.validation_passed() {
        let errors: Vec<String> = validated.validation_errors().take(5).map(|e| format!("{} {e}", e.policy_id())).collect();
        return Err(format!("cedar policies do not validate against the schema: {}", errors.join("; ")));
    }
    let mut digest = Sha256::new();
    for (n, b) in &named {
        digest.update(n.as_bytes());
        digest.update([0]);
        digest.update(b.as_bytes());
        digest.update([0]);
    }
    let id = digest.finalize().iter().take(8).fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    Ok(Loaded { named, id, policies, schema, types })
}

/// The entity types of a schema in its JSON form: their attributes and parents. Namespaced
/// schemas are not taken: a subject `type:id` cannot name `App::User`.
fn shapes_of(json: &Value) -> Result<HashMap<String, Shape>, String> {
    let spaces = json.as_object().ok_or("cedar schema: not an object")?;
    if let Some(ns) = spaces.keys().find(|k| !k.is_empty()) {
        return Err(format!("cedar schema: namespace {ns}: pg_auth takes types without a namespace"));
    }
    let Some(space) = spaces.get("") else { return Ok(HashMap::new()) };
    let entity_types = space["entityTypes"].as_object().cloned().unwrap_or_default();
    let common = space["commonTypes"].as_object().cloned().unwrap_or_default();
    let kind_of = |t: &Value| kind(t, &entity_types, &common, 0);
    Ok(entity_types
        .iter()
        .map(|(name, decl)| {
            let attributes = decl["shape"]["attributes"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(a, t)| (a.clone(), (kind_of(t), t.get("required").and_then(Value::as_bool).unwrap_or(true))))
                .collect();
            let member_of = decl["memberOfTypes"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            (name.clone(), Shape { attributes, member_of })
        })
        .collect())
}

fn kind(t: &Value, entities: &Map<String, Value>, common: &Map<String, Value>, depth: usize) -> Kind {
    if depth > 8 {
        return Kind::Other;
    }
    let name = t["name"].as_str().unwrap_or_default();
    match t["type"].as_str().unwrap_or_default() {
        "String" => Kind::String,
        "Long" => Kind::Long,
        "Boolean" | "Bool" => Kind::Bool,
        "Entity" => Kind::Entity(name.to_owned()),
        "Set" => Kind::Set(Box::new(kind(&t["element"], entities, common, depth + 1))),
        "Record" => Kind::Record(
            t["attributes"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(a, at)| {
                    (a.clone(), (kind(at, entities, common, depth + 1), at.get("required").and_then(Value::as_bool).unwrap_or(true)))
                })
                .collect(),
        ),
        "EntityOrCommon" if entities.contains_key(name) => Kind::Entity(name.to_owned()),
        "EntityOrCommon" if common.contains_key(name) => kind(&common[name], entities, common, depth + 1),
        "EntityOrCommon" => match name {
            "String" => Kind::String,
            "Long" => Kind::Long,
            "Bool" | "Boolean" => Kind::Bool,
            _ => Kind::Other,
        },
        _ => Kind::Other,
    }
}

// ---- entities

fn euid((ty, id): &Uid) -> Result<EntityUid, String> {
    Ok(EntityUid::from_type_name_and_id(type_name(ty)?, cedar_policy::EntityId::new(id)))
}

fn type_name(ty: &str) -> Result<EntityTypeName, String> {
    EntityTypeName::from_str(ty).map_err(|e| format!("cedar: type {ty}: {e}"))
}

fn action(permission: &str) -> Result<EntityUid, String> {
    Ok(EntityUid::from_type_name_and_id(type_name("Action")?, cedar_policy::EntityId::new(permission)))
}

/// Whether the schema lets a `child` be `in` a `parent`.
fn may_be_in(loaded: &Loaded, child: &str, parent: &str) -> bool {
    loaded.types.get(child).is_some_and(|s| s.member_of.iter().any(|t| t == parent))
}

fn ancestors(loaded: &Loaded, store: &Store, uid: &Uid) -> HashSet<Uid> {
    Store::reach(&store.parents, uid, |child, parent| may_be_in(loaded, &child.0, &parent.0))
}

fn descendants(loaded: &Loaded, store: &Store, uid: &Uid) -> HashSet<Uid> {
    Store::reach(&store.children, uid, |parent, child| may_be_in(loaded, &child.0, &parent.0))
}

/// The ancestors of `uid` as entities, each with its own ancestors and its attributes unknown;
/// those of a type the schema does not declare are left out.
fn ancestors_json(loaded: &Loaded, store: &Store, uid: &Uid) -> Vec<Value> {
    ancestors(loaded, store, uid)
        .into_iter()
        .map(|a| json!({"uid": entity_json(&a), "parents": ancestors(loaded, store, &a).iter().map(entity_json).collect::<Vec<_>>()}))
        .collect()
}

fn roles_of(principal: Option<&Value>) -> Vec<String> {
    principal.and_then(|p| p["roles"].as_array()).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
}

/// Every ancestor of the principal: from the store, its roles when its type may be `in` a `Role`,
/// and theirs. None when they cannot be known, a principal of such a type given without its roles:
/// Cedar then keeps every `principal in` open rather than take a set that may lack one.
fn principal_ancestors(loaded: &Loaded, store: &Store, uid: &Uid, principal: Option<&Value>) -> Option<HashSet<Uid>> {
    let mut all = ancestors(loaded, store, uid);
    if may_be_in(loaded, &uid.0, "Role") {
        for role in roles_of(Some(principal?)) {
            let role = ("Role".to_owned(), role);
            all.extend(ancestors(loaded, store, &role));
            all.insert(role);
        }
    }
    Some(all)
}

/// The principal with its ancestors (`principal_ancestors`; unknown when None), each with its own;
/// its attributes from the principal's (claims, with `roles`), when every required one is there,
/// else unknown.
fn principal_entities(
    loaded: &Loaded,
    store: &Store,
    uid: &Uid,
    principal: Option<&Value>,
    ancestors_of: Option<&HashSet<Uid>>,
) -> Vec<Value> {
    let mut entity = Map::new();
    entity.insert("uid".into(), entity_json(uid));
    if let Some(all) = ancestors_of {
        entity.insert("parents".into(), Value::Array(all.iter().map(entity_json).collect()));
    }
    if let Some(p) = principal {
        let mut given = p["attr"].as_object().cloned().unwrap_or_default();
        given.entry("roles").or_insert_with(|| json!(roles_of(principal)));
        if let Some(attrs) = attributes(loaded, &uid.0, &Value::Object(given)) {
            entity.insert("attrs".into(), attrs);
        }
    }
    let mut all = vec![Value::Object(entity)];
    all.extend(
        ancestors_of
            .into_iter()
            .flatten()
            .map(|a| json!({"uid": entity_json(a), "parents": ancestors(loaded, store, a).iter().map(entity_json).collect::<Vec<_>>()})),
    );
    all
}

/// An entity's attributes from `given` (a row's, a principal's), as the schema types them: only
/// those it declares; None when a required one is absent (null) or not of its type (the entity's
/// attributes are then unknown to Cedar).
fn attributes(loaded: &Loaded, ty: &str, given: &Value) -> Option<Value> {
    let shape = loaded.types.get(ty)?;
    let mut out = Map::new();
    for (name, (kind, required)) in &shape.attributes {
        match given.get(name).filter(|v| !v.is_null()) {
            Some(v) => {
                out.insert(name.clone(), value_of(kind, v)?);
            }
            None if *required => return None,
            None => {}
        }
    }
    Some(Value::Object(out))
}

fn value_of(kind: &Kind, v: &Value) -> Option<Value> {
    Some(match (kind, v) {
        // as the column holds it (authz_internal.rules_problems refuses a column of another type):
        // no conversion the compiled filter would not make too
        (Kind::String, Value::String(_)) | (Kind::Bool, Value::Bool(_)) => v.clone(),
        (Kind::Long, Value::Number(n)) => json!(n.as_i64()?),
        (Kind::Entity(t), Value::String(id)) => json!({"__entity": {"type": t, "id": id}}),
        (Kind::Entity(t), Value::Number(n)) => json!({"__entity": {"type": t, "id": n.to_string()}}),
        (Kind::Set(k), Value::Array(items)) => Value::Array(items.iter().map(|i| value_of(k, i)).collect::<Option<_>>()?),
        // its declared attributes (a token's app_metadata, say): a required one absent, none
        (Kind::Record(attributes), Value::Object(given)) => {
            let mut out = Map::new();
            for (name, (k, required)) in attributes {
                match given.get(name).filter(|v| !v.is_null()) {
                    Some(v) => {
                        out.insert(name.clone(), value_of(k, v)?);
                    }
                    None if *required => return None,
                    None => {}
                }
            }
            Value::Object(out)
        }
        _ => return None,
    })
}

/// Entities named more than once (a resource among the principal's ancestors) kept once: the
/// first, which callers put fullest first.
fn dedup(entities: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    entities.into_iter().filter(|e| seen.insert(e["uid"].to_string())).collect()
}

/// The required attributes of a type: present on every row a condition allows.
fn required(loaded: &Loaded, ty: &str) -> Vec<String> {
    loaded.types.get(ty).map(|s| s.attributes.iter().filter(|(_, (_, r))| *r).map(|(n, _)| n.clone()).collect()).unwrap_or_default()
}

// ---- residuals into the protocol's language

/// A condition while it is built: constants folded, a part outside the language left to Cedar.
#[derive(Clone, Debug, PartialEq)]
enum Cond {
    True,
    False,
    /// ask Cedar about the row
    Check,
    Expr(Value),
}

impl Cond {
    fn value(self) -> Value {
        match self {
            Cond::Expr(e) => e,
            Cond::Check => json!({"op": "check", "args": []}),
            // folded away by the constructors before they get here
            Cond::True => json!({"op": "eq", "args": [{"value": 1}, {"value": 1}]}),
            Cond::False => json!({"op": "eq", "args": [{"value": 1}, {"value": 2}]}),
        }
    }

    fn and(a: Cond, b: Cond) -> Cond {
        match (a, b) {
            (Cond::False, _) | (_, Cond::False) => Cond::False,
            (Cond::True, x) | (x, Cond::True) => x,
            (a, b) => Cond::Expr(json!({"op": "and", "args": [a.value(), b.value()]})),
        }
    }

    fn or(a: Cond, b: Cond) -> Cond {
        match (a, b) {
            (Cond::True, _) | (_, Cond::True) => Cond::True,
            (Cond::False, x) | (x, Cond::False) => x,
            (Cond::Check, Cond::Check) => Cond::Check,
            // the rest first: Cedar is asked only about the rows it leaves undecided
            (Cond::Check, e) | (e, Cond::Check) => Cond::Expr(json!({"op": "or", "args": [e.value(), Cond::Check.value()]})),
            (a, b) => Cond::Expr(json!({"op": "or", "args": [a.value(), b.value()]})),
        }
    }

    fn not(a: Cond) -> Cond {
        match a {
            Cond::True => Cond::False,
            Cond::False => Cond::True,
            Cond::Check => Cond::Check,
            e @ Cond::Expr(_) => Cond::Expr(json!({"op": "not", "args": [e.value()]})),
        }
    }
}

/// The expression of a residual policy (its one `when` clause, in Cedar's JSON form).
fn residual_body(policy: &Value) -> Option<Value> {
    match policy["conditions"].as_array()?.as_slice() {
        [] => Some(json!({"Value": true})),
        [c] if c["kind"] == "when" => Some(c["body"].clone()),
        _ => None,
    }
}

struct Translate<'a> {
    resource_type: &'a str,
    loaded: &'a Loaded,
    store: &'a Store,
    limit: usize,
    principal: &'a Uid,
    /// its ancestors with its roles' (`principal_ancestors`), None when unknown
    principal_ancestors: Option<&'a HashSet<Uid>>,
}

/// An operand: the row's attribute or id, or a value.
enum Operand {
    Row(Value),
    /// the resource itself, compared with an entity
    Id,
    Value(Value),
    Entity(Uid),
}

impl Translate<'_> {
    /// The condition `e` (Cedar JSON) says, or None when it is outside the language.
    fn cond(&self, expr: &Value) -> Option<Cond> {
        let (op, args) = expr.as_object().filter(|o| o.len() == 1)?.iter().next()?;
        match op.as_str() {
            "Value" => args.as_bool().map(|b| if b { Cond::True } else { Cond::False }),
            // a boolean attribute as the condition itself
            "." if is_resource(&args["left"]) => {
                Some(Cond::Expr(json!({"op": "eq", "args": [{"column": args["attr"].as_str()?}, {"value": true}]})))
            }
            "&&" => Some(Cond::and(self.cond(&args["left"])?, self.cond(&args["right"])?)),
            "||" => Some(Cond::or(self.cond(&args["left"])?, self.cond(&args["right"])?)),
            "!" => Some(Cond::not(self.cond(&args["arg"])?)),
            "if-then-else" => {
                let (test, then, otherwise) = (self.cond(&args["if"])?, self.cond(&args["then"])?, self.cond(&args["else"])?);
                Some(Cond::or(Cond::and(test.clone(), then), Cond::and(Cond::not(test), otherwise)))
            }
            "==" | "!=" | "<" | "<=" | ">" | ">=" => self.compare(op, &args["left"], &args["right"]),
            "has" if is_resource(&args["left"]) => {
                Some(Cond::Expr(json!({"op": "ne", "args": [{"column": args["attr"].as_str()?}, {"value": null}]})))
            }
            "is" if is_resource(&args["left"]) && args.get("in").is_none() => {
                Some(if args["entity_type"].as_str()? == self.resource_type { Cond::True } else { Cond::False })
            }
            // [literals].contains(row operand)
            "contains" => {
                let items = args["left"]["Set"].as_array()?;
                let values = items.iter().map(literal).collect::<Option<Vec<_>>>()?;
                // entities by their ids, marked so; a set mixing entities and values is left to Cedar
                let entities = items.iter().filter(|i| entity_literal(i).is_some()).count();
                let list = match entities {
                    0 => json!({"value": values}),
                    n if n == items.len() => json!({"value": values, "entity": true}),
                    _ => return None,
                };
                match operand(&args["right"])? {
                    Operand::Row(_) if values.is_empty() => Some(Cond::False),
                    Operand::Row(row) => Some(Cond::Expr(json!({"op": "in", "args": [row, list]}))),
                    _ => None,
                }
            }
            "in" if is_resource(&args["left"]) => {
                let ancestors: Vec<Uid> = match &args["right"] {
                    Value::Object(o) if o.contains_key("Set") => {
                        o["Set"].as_array()?.iter().map(entity_literal).collect::<Option<Vec<_>>>()?
                    }
                    one => vec![entity_literal(one)?],
                };
                let mut ids: Vec<String> = Vec::new();
                for anc in &ancestors {
                    if anc.0 == self.resource_type {
                        ids.push(anc.1.clone());
                    }
                    ids.extend(descendants(self.loaded, self.store, anc).into_iter().filter(|d| d.0 == self.resource_type).map(|d| d.1));
                }
                ids.sort();
                ids.dedup();
                if ids.len() > self.limit {
                    return None;
                }
                Some(if ids.is_empty() {
                    Cond::False
                } else {
                    Cond::Expr(json!({"op": "in", "args": [{"id": true}, {"value": ids, "entity": true}]}))
                })
            }
            // a known entity (the principal) in the resource, or in an entity attribute of it:
            // the id, or the column, among that entity and its ancestors of the type it holds
            "in" => {
                let entity = entity_literal(&args["left"])?;
                let (row, target) = match operand(&args["right"])? {
                    Operand::Id => (json!({"id": true}), self.resource_type.to_owned()),
                    Operand::Row(row) => {
                        let attribute = row["column"].as_str()?;
                        match &self.loaded.types.get(self.resource_type)?.attributes.get(attribute)?.0 {
                            Kind::Entity(t) => (row, t.clone()),
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
                let above =
                    if &entity == self.principal { self.principal_ancestors?.clone() } else { ancestors(self.loaded, self.store, &entity) };
                let mut ids: Vec<String> = above.into_iter().filter(|a| a.0 == target).map(|a| a.1).collect();
                if entity.0 == target {
                    ids.push(entity.1);
                }
                ids.sort();
                ids.dedup();
                if ids.len() > self.limit {
                    return None;
                }
                Some(if ids.is_empty() {
                    Cond::False
                } else {
                    Cond::Expr(json!({"op": "in", "args": [row, {"value": ids, "entity": true}]}))
                })
            }
            _ => None,
        }
    }

    fn compare(&self, op: &str, left: &Value, right: &Value) -> Option<Cond> {
        let name = match op {
            "==" => "eq",
            "!=" => "ne",
            "<" => "lt",
            "<=" => "le",
            ">" => "gt",
            _ => "ge",
        };
        let (l, r) = (operand(left)?, operand(right)?);
        match (l, r) {
            // the resource compared with an entity: its id, when the entity is of its type
            (Operand::Id, Operand::Entity(e)) | (Operand::Entity(e), Operand::Id) if matches!(name, "eq" | "ne") => {
                let same = e.0 == self.resource_type;
                Some(match (same, name) {
                    (true, _) => Cond::Expr(json!({"op": name, "args": [{"id": true}, {"value": e.1, "entity": true}]})),
                    (false, "eq") => Cond::False,
                    (false, _) => Cond::True,
                })
            }
            (Operand::Row(row), other) => Some(Cond::Expr(json!({"op": name, "args": [row, plain(other)?]}))),
            (other, Operand::Row(row)) => Some(Cond::Expr(json!({"op": name, "args": [plain(other)?, row]}))),
            _ => None,
        }
    }
}

fn operand(e: &Value) -> Option<Operand> {
    let (op, a) = e.as_object().filter(|o| o.len() == 1)?.iter().next()?;
    match op.as_str() {
        "." if is_resource(&a["left"]) => Some(Operand::Row(json!({"column": a["attr"].as_str()?}))),
        "Var" if a == "resource" => Some(Operand::Id),
        "Value" => match entity_literal(e) {
            Some(uid) => Some(Operand::Entity(uid)),
            None => literal(e).map(Operand::Value),
        },
        _ => None,
    }
}

/// A value as the protocol takes it: an entity by its id, marked as one.
fn plain(o: Operand) -> Option<Value> {
    match o {
        Operand::Value(v) => Some(json!({"value": v})),
        Operand::Entity((_, id)) => Some(json!({"value": id, "entity": true})),
        Operand::Row(_) | Operand::Id => None,
    }
}

fn is_resource(e: &Value) -> bool {
    e.get("Var").and_then(Value::as_str) == Some("resource")
}

/// A literal string, number or boolean; an entity literal as its id.
fn literal(e: &Value) -> Option<Value> {
    match e.get("Value")? {
        v @ (Value::String(_) | Value::Bool(_)) => Some(v.clone()),
        Value::Number(n) => Some(json!(n.as_i64()?)),
        Value::Object(o) => o.get("__entity").and_then(|x| x["id"].as_str()).map(|id| json!(id)),
        _ => None,
    }
}

fn entity_literal(e: &Value) -> Option<Uid> {
    let ent = e.get("Value")?.get("__entity")?;
    Some((ent["type"].as_str()?.to_owned(), ent["id"].as_str()?.to_owned()))
}

/// The ids a condition allows when it allows by id only (`in` and `eq` on the id, or'd).
fn ids_only(c: &Value) -> Option<HashSet<String>> {
    let args = c["args"].as_array()?;
    match (c["op"].as_str()?, args.as_slice()) {
        ("or", _) => args.iter().try_fold(HashSet::new(), |mut all, a| {
            all.extend(ids_only(a)?);
            Some(all)
        }),
        ("in", [id, list]) if id["id"] == true => {
            list["value"].as_array().map(|l| l.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        }
        ("eq", [id, v]) if id["id"] == true => v["value"].as_str().map(|s| HashSet::from([s.to_owned()])),
        _ => None,
    }
}

// ---- the parents, kept in Postgres

/// Keep this sidecar's parents those Postgres keeps (authz_internal.entity_parents, written by
/// whichever sidecar delivers the relationship tables, outbox.rs): read when it starts, after every
/// delivery (NOTIFY pg_auth_delivered), and every `every` in case one was missed. `changed` is
/// called after each read (the sidecar forgets the answers it cached).
pub fn mirror(url: String, engine: Arc<crate::engine::Engine>, every: std::time::Duration, changed: impl Fn() + Send + Sync + 'static) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = mirror_once(&url, &engine, every, &changed).await {
                eprintln!("pg-auth-sidecar: cedar parents: {e}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

async fn mirror_once(
    url: &str,
    engine: &crate::engine::Engine,
    every: std::time::Duration,
    changed: &(impl Fn() + Send + Sync),
) -> Result<(), String> {
    use futures_util::StreamExt;
    let Some(cedar) = engine.cedar() else { return Ok(()) };
    let (client, mut connection) = crate::pg::connect(url).await?;
    let (wake, mut woken) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(message) = messages.next().await {
            match message {
                Ok(tokio_postgres::AsyncMessage::Notification(_)) => {
                    let _ = wake.send(());
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    let pg = |e: tokio_postgres::Error| format!("postgres: {e}");
    client.batch_execute("listen pg_auth_delivered").await.map_err(pg)?;
    let mut count = None;
    loop {
        // a change applied while reading: read again, so the store never goes back in time
        loop {
            let since = cedar.version();
            let rows = client.query("select child, parent from authz_sidecar.entity_parents()", &[]).await.map_err(pg)?;
            let rows: Vec<(String, String, String)> = rows.iter().map(|r| (r.get(0), "in".to_owned(), r.get(1))).collect();
            if cedar.replace(&rows, since)? {
                if count != Some(rows.len()) {
                    eprintln!("pg-auth-sidecar: cedar: {} parent relationships", rows.len());
                    count = Some(rows.len());
                }
                break;
            }
        }
        changed();
        tokio::select! {
            _ = woken.recv() => {}
            () = cedar.stale.notified() => {}
            () = tokio::time::sleep(every) => {}
        }
        // a burst of deliveries read once
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        while woken.try_recv().is_ok() {}
        if client.is_closed() {
            return Err("postgres connection closed".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA_TEXT: &str = r"
        entity Role;
        entity Group;
        entity User in [Group, Role] { dept: String, level: Long };
        entity Folder in [Folder];
        entity Document in [Folder] { owner: User, status?: String, size: Long, public: Bool, tags: Set<String> };
        action view, edit appliesTo { principal: User, resource: Document };
    ";

    const POLICIES: &str = r#"
        permit(principal, action == Action::"view", resource) when { resource.owner == principal };
        permit(principal, action == Action::"view", resource) when { resource.public && resource.size < 10 };
        permit(principal in Group::"eng", action == Action::"view", resource in Folder::"f1");
        permit(principal, action == Action::"view", resource) when { resource has status && ["draft", "review"].contains(resource.status) && principal.level > 2 };
        permit(principal, action == Action::"view", resource) when { resource.tags.contains(principal.dept) };
        forbid(principal, action == Action::"view", resource) when { resource has status && resource.status == "secret" };
        permit(principal in Role::"admin", action == Action::"edit", resource);
    "#;

    fn change(object: &str, subject: &str) -> Change {
        Change { write: true, object: object.into(), relation: "in".into(), subject: subject.into(), ..Change::default() }
    }

    fn cedar() -> Cedar {
        let c = Cedar::new();
        let rules = [("schema", SCHEMA_TEXT), ("policies", POLICIES)].map(|(n, b)| (n.to_owned(), b.to_owned()));
        assert!(c.load(BTreeMap::from(rules)).unwrap());
        c.apply(&[
            change("User:anne", "Group:eng"),
            change("User:bob", "Group:sales"),
            change("Folder:f2", "Folder:f1"),
            change("Document:1", "Folder:f2"),
            change("Document:2", "Folder:f3"),
            change("Document:3", "Folder:f1"),
        ])
        .unwrap();
        c
    }

    fn anne() -> Value {
        json!({"roles": ["member"], "attr": {"dept": "eng", "level": 3}})
    }

    /// The protocol's condition evaluated on a row, as the extension evaluates it (condition.rs):
    /// a `check` asks `check`.
    fn eval(c: &Value, id: &str, row: &Value, check: &dyn Fn() -> bool) -> bool {
        let args = c["args"].as_array().cloned().unwrap_or_default();
        let operand = |a: &Value| -> Value {
            if a["id"] == true {
                json!(id)
            } else if let Some(col) = a["column"].as_str() {
                row.get(col).cloned().unwrap_or(Value::Null)
            } else {
                a["value"].clone()
            }
        };
        let compare = |l: &Value, r: &Value| match (l, r) {
            (Value::Number(a), Value::Number(b)) => a.as_i64().partial_cmp(&b.as_i64()),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
            _ => None,
        };
        match c["op"].as_str().unwrap_or_default() {
            "and" => args.iter().all(|a| eval(a, id, row, check)),
            "or" => args.iter().any(|a| eval(a, id, row, check)),
            "not" => !eval(&args[0], id, row, check),
            "check" => check(),
            "eq" => text(&operand(&args[0])) == text(&operand(&args[1])),
            "ne" => text(&operand(&args[0])) != text(&operand(&args[1])),
            "in" => operand(&args[1]).as_array().is_some_and(|l| l.iter().any(|i| text(i) == text(&operand(&args[0])))),
            op => {
                let o = compare(&operand(&args[0]), &operand(&args[1]));
                match op {
                    "lt" => o == Some(std::cmp::Ordering::Less),
                    "le" => matches!(o, Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)),
                    "gt" => o == Some(std::cmp::Ordering::Greater),
                    _ => matches!(o, Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)),
                }
            }
        }
    }

    /// Values as the extension compares an entity's id: by text form (the tests' rows give ids as
    /// strings, and numbers only where the schema has a Long, compared with Longs).
    fn text(v: &Value) -> Option<String> {
        match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    fn rows() -> Vec<(String, Value)> {
        let owners = ["anne", "bob"];
        let statuses = [Value::Null, json!("draft"), json!("secret"), json!("final")];
        let mut rows = Vec::new();
        let mut n = 0;
        for owner in owners {
            for status in &statuses {
                for size in [3, 30] {
                    for public in [true, false] {
                        for tags in [json!([]), json!(["eng"]), json!(["ops"]), Value::Null] {
                            n += 1;
                            let id = (n % 5 + 1).to_string();
                            rows.push((id, json!({"owner": owner, "status": status, "size": size, "public": public, "tags": tags})));
                        }
                    }
                }
            }
        }
        rows
    }

    #[test]
    fn a_plan_allows_the_rows_cedar_allows() {
        let c = cedar();
        for (subject, principal) in [("User:anne", anne()), ("User:bob", json!({"roles": [], "attr": {"dept": "sales", "level": 1}}))] {
            let plan = c.plan(subject, "view", "Document", Some(&principal), 1000).unwrap();
            for (id, row) in rows() {
                let cedar_says = c.check(subject, "view", "Document", &[&id], &[Some(&row)], Some(&principal)).unwrap()[0];
                let check = || cedar_says;
                let plan_says = match &plan {
                    Plan::Always => true,
                    Plan::Never => false,
                    Plan::Rows => cedar_says,
                    Plan::Condition(cond) => eval(cond, &id, &row, &check),
                };
                assert_eq!(plan_says, cedar_says, "{subject} document {id} {row}: plan {plan:?}");
            }
        }
    }

    #[test]
    fn a_batch_decides_as_its_rows_one_by_one() {
        let c = cedar();
        let principal = anne();
        let mut all = rows();
        // an id again, with other attributes: decided on its own
        all.push(("2".into(), json!({"owner": "anne", "size": 3, "public": true, "tags": []})));
        all.push(("2".into(), json!({"owner": "bob", "size": 30, "public": false, "tags": []})));
        let ids: Vec<&String> = all.iter().map(|(id, _)| id).collect();
        let attrs: Vec<Option<&Value>> = all.iter().map(|(_, row)| Some(row)).collect();
        let batch = c.check("User:anne", "view", "Document", &ids, &attrs, Some(&principal)).unwrap();
        let one_by_one: Vec<bool> = all
            .iter()
            .map(|(id, row)| c.check("User:anne", "view", "Document", &[id], &[Some(row)], Some(&principal)).unwrap()[0])
            .collect();
        assert_eq!(batch, one_by_one);
        assert_ne!(batch[batch.len() - 2], batch[batch.len() - 1], "the same id, other attributes, another answer");
    }

    #[test]
    fn attributes_become_columns_and_parents_become_ids() {
        let c = cedar();
        let Plan::Condition(plan) = c.plan("User:anne", "view", "Document", Some(&anne()), 1000).unwrap() else { panic!("a condition") };
        let text = plan.to_string();
        // resource.owner == principal: the column against the principal's id
        assert!(text.contains(r#"{"op":"eq","args":[{"column":"owner"},{"value":"anne","entity":true}]}"#), "{text}");
        // anne is in eng (the store): resource in Folder::"f1" is the documents below f1
        assert!(text.contains(r#""args":[{"id":true},{"value":["1","3"],"entity":true}]"#), "{text}");
        // tags.contains: not in the language, Cedar is asked about the rows the rest leaves
        assert!(text.contains(r#"{"op":"check","args":[]}"#), "{text}");
        // the forbid, and the required attributes present
        assert!(text.contains(r#"{"op":"not""#) && text.contains(r#"{"op":"ne","args":[{"column":"size"},{"value":null}]}"#), "{text}");
    }

    #[test]
    fn roles_are_parents_when_the_schema_says_so() {
        let c = cedar();
        let admin = json!({"roles": ["admin"], "attr": {"dept": "x", "level": 0}});
        // every document an admin's, but for one Cedar would not take (a required attribute missing)
        let Plan::Condition(only_present) = c.plan("User:carl", "edit", "Document", Some(&admin), 1000).unwrap() else {
            panic!("a condition")
        };
        let text = only_present.to_string();
        assert!(text.contains(r#"{"value":null}"#) && !text.contains("check") && !text.contains(r#""op":"in""#), "{text}");
        assert_eq!(c.plan("User:carl", "edit", "Document", Some(&anne()), 1000).unwrap(), Plan::Never);
    }

    #[test]
    fn listing_answers_ids_when_the_policies_name_them() {
        let c = Cedar::new();
        // a principal that cannot be in a Role: its parents are the store's, known without its token
        let schema = "entity Group; entity User in [Group]; entity Folder in [Folder]; entity Document in [Folder];
                      action view appliesTo { principal: User, resource: Document };";
        let rules =
            [("schema", schema), ("p", r#"permit(principal in Group::"eng", action == Action::"view", resource in Folder::"f1");"#)];
        c.load(BTreeMap::from(rules.map(|(n, b)| (n.to_owned(), b.to_owned())))).unwrap();
        c.apply(&[change("User:anne", "Group:eng"), change("Folder:f2", "Folder:f1"), change("Document:1", "Folder:f2")]).unwrap();
        let Listed::Ids(ids) = c.list("User:anne", "view", "Document", 1000).unwrap() else { panic!("ids") };
        assert_eq!(ids, ["1"]);
        assert!(matches!(c.list("User:bob", "view", "Document", 1000).unwrap(), Listed::Ids(ids) if ids.is_empty()));
        // more than the limit: rows are checked
        assert!(matches!(c.list("User:anne", "view", "Document", 0).unwrap(), Listed::TooMany));
        // revoked: gone at once
        c.apply(&[Change { write: false, ..change("Document:1", "Folder:f2") }]).unwrap();
        assert!(matches!(c.list("User:anne", "view", "Document", 1000).unwrap(), Listed::Ids(ids) if ids.is_empty()));
    }

    #[test]
    fn rules_that_do_not_validate_are_refused_and_the_ones_before_kept() {
        let c = cedar();
        let bad = Rule {
            name: "policies".into(),
            body: r"permit(principal, action, resource) when { resource.nope == 1 };".into(),
            previous: None,
        };
        let refused = c.push(&[bad]).unwrap_err();
        assert!(refused.permanent && refused.message.contains("validate"), "{}", refused.message);
        assert!(
            matches!(c.plan("User:anne", "view", "Document", Some(&anne()), 1000), Ok(Plan::Condition(_))),
            "the rules before still answer"
        );
        assert!(
            compile(BTreeMap::from([("p".to_owned(), "permit(principal, action, resource);".to_owned())]))
                .err()
                .unwrap_or_default()
                .contains("no schema")
        );
        let namespaced = "namespace App { entity User; }";
        assert!(compile(BTreeMap::from([("schema".to_owned(), namespaced.to_owned())])).err().unwrap_or_default().contains("namespace"));
    }

    #[test]
    fn a_relationship_is_an_in_without_a_condition() {
        let c = cedar();
        let other = Change { relation: "viewer".into(), ..change("Document:1", "User:anne") };
        assert!(c.apply(&[other]).unwrap_err().permanent);
        assert!(c.apply(&[change("Document", "Folder:f1")]).unwrap_err().message.contains("<type>:<id>"));
    }

    #[test]
    fn a_reload_that_raced_a_change_is_not_taken() {
        let c = cedar();
        let since = c.version();
        c.apply(&[change("Document:9", "Folder:f1")]).unwrap();
        assert!(!c.replace(&[], since).unwrap(), "read before the change: read again");
        assert!(c.replace(&[("Document:9".into(), "in".into(), "Folder:f1".into())], c.version()).unwrap());
    }
}

#[cfg(test)]
mod tenant_tests {
    use super::*;

    #[test]
    fn a_principal_in_the_resources_org_becomes_the_org_column_among_its_orgs() {
        let c = Cedar::new();
        let schema = "entity Org; entity User in [Org, Document]; entity Document { org: Org };
                      action view appliesTo { principal: User, resource: Document };";
        let policies = r#"permit (principal, action == Action::"view", resource) when { principal in resource.org };
                          permit (principal, action == Action::"view", resource) when { principal in resource };"#;
        c.load(BTreeMap::from([("schema".to_owned(), schema.to_owned()), ("p".to_owned(), policies.to_owned())])).unwrap();
        let w = |o: &str, s: &str| Change { write: true, object: o.into(), relation: "in".into(), subject: s.into(), ..Change::default() };
        c.apply(&[w("User:anne", "Org:1"), w("User:anne", "Org:3"), w("User:anne", "Document:7")]).unwrap();
        let Plan::Condition(plan) = c.plan("User:anne", "view", "Document", None, 1000).unwrap() else { panic!("a condition") };
        let text = plan.to_string();
        assert!(text.contains(r#"[{"column":"org"},{"value":["1","3"],"entity":true}]"#), "{text}");
        assert!(text.contains(r#"[{"id":true},{"value":["7"],"entity":true}]"#), "{text}");
        // and Cedar agrees, row by row
        for (id, org, allowed) in [("1", "1", true), ("2", "2", false), ("7", "2", true), ("8", "3", true)] {
            let row = json!({"org": org});
            assert_eq!(c.check("User:anne", "view", "Document", &[&id.to_owned()], &[Some(&row)], None).unwrap(), [allowed], "{id}");
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn load(schema: &str, policies: &str) -> Cedar {
        let c = Cedar::new();
        c.load(BTreeMap::from([("schema".to_owned(), schema.to_owned()), ("p".to_owned(), policies.to_owned())])).unwrap();
        c
    }

    fn edge(child: &str, parent: &str) -> Change {
        Change { write: true, object: child.into(), relation: "in".into(), subject: parent.into(), ..Change::default() }
    }

    fn condition(plan: Plan) -> String {
        match plan {
            Plan::Condition(c) => c.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn has_is_a_column_present_and_an_entity_list_keeps_its_marker() {
        let c = load(
            "entity User; entity Doc { approved_by?: User, owner: User, public: Bool };
             action view appliesTo { principal: User, resource: Doc };",
            r#"permit (principal, action, resource) when { resource has approved_by };
               permit (principal, action, resource) when { resource.public };
               forbid (principal, action, resource) when { [User::"1", User::"2"].contains(resource.owner) };"#,
        );
        let plan = condition(c.plan("User:anne", "view", "Doc", None, 1000).unwrap());
        assert!(plan.contains(r#"{"op":"ne","args":[{"column":"approved_by"},{"value":null}]}"#), "{plan}");
        assert!(plan.contains(r#"{"op":"eq","args":[{"column":"public"},{"value":true}]}"#), "{plan}");
        assert!(plan.contains(r#"[{"column":"owner"},{"value":["1","2"],"entity":true}]"#), "{plan}");
    }

    const ROLES: &str = "entity Role in [Role]; entity User in [Role]; entity Doc { embargo: Role };
                         action view appliesTo { principal: User, resource: Doc };";

    #[test]
    fn a_token_roles_ancestors_count() {
        let c = load(ROLES, r#"permit (principal, action, resource); forbid (principal in Role::"restricted", action, resource);"#);
        c.apply(&[edge("Role:contractor", "Role:restricted")]).unwrap();
        let contractor = json!({"roles": ["contractor"], "attr": {}});
        assert_eq!(c.plan("User:anne", "view", "Doc", Some(&contractor), 1000).unwrap(), Plan::Never);
        assert_eq!(
            c.check("User:anne", "view", "Doc", &[&"1".to_owned()], &[Some(&json!({"embargo": "x"}))], Some(&contractor)).unwrap(),
            [false]
        );
        // no principal given: its roles are unknown, so nothing is decided on them
        assert!(matches!(c.list("User:anne", "view", "Doc", 1000).unwrap(), Listed::TooMany));
        assert_eq!(c.check("User:anne", "view", "Doc", &[&"1".to_owned()], &[None], None).unwrap(), [false]);
    }

    #[test]
    fn a_principal_in_an_attribute_counts_its_token_roles() {
        let c = load(
            ROLES,
            r"permit (principal, action, resource); forbid (principal, action, resource) when { principal in resource.embargo };",
        );
        let contractor = json!({"roles": ["contractor"], "attr": {}});
        let plan = condition(c.plan("User:anne", "view", "Doc", Some(&contractor), 1000).unwrap());
        assert!(plan.contains(r#"[{"column":"embargo"},{"value":["contractor"],"entity":true}]"#), "{plan}");
        for (embargo, allowed) in [("contractor", false), ("staff", true)] {
            let row = json!({"embargo": embargo});
            assert_eq!(c.check("User:anne", "view", "Doc", &[&"1".to_owned()], &[Some(&row)], Some(&contractor)).unwrap(), [allowed]);
        }
    }

    #[test]
    fn a_principals_record_attribute_comes_from_its_claims() {
        let c = load(
            "entity User { app_metadata: { role?: String } }; entity Todo { owner_id: User };
             action read appliesTo { principal: User, resource: Todo };",
            r#"permit (principal, action, resource) when { resource.owner_id == principal || (principal.app_metadata has role && ["admin", "support"].contains(principal.app_metadata.role)) };"#,
        );
        let admin = json!({"roles": [], "attr": {"app_metadata": {"role": "admin"}}});
        let member = json!({"roles": [], "attr": {"app_metadata": {}}});
        // an admin: every row Cedar takes (its required owner present)
        let everything = condition(c.plan("User:a", "read", "Todo", Some(&admin), 1000).unwrap());
        assert_eq!(everything, r#"{"op":"ne","args":[{"column":"owner_id"},{"value":null}]}"#);
        let own = condition(c.plan("User:b", "read", "Todo", Some(&member), 1000).unwrap());
        assert!(own.contains(r#"{"op":"eq","args":[{"column":"owner_id"},{"value":"b","entity":true}]}"#), "{own}");
    }

    #[tokio::test]
    async fn a_hierarchy_fills_the_index_and_its_changes_make_it_stale() {
        let c = load(
            "entity User; entity Folder in [Folder, User]; action read appliesTo { principal: User, resource: Folder };",
            r#"permit (principal, action == Action::"read", resource) when { resource in principal };"#,
        );
        let tree: Vec<Change> =
            (1..=1500).map(|i| edge(&format!("Folder:{i}"), "Folder:0")).chain([edge("Folder:0", "User:anne")]).collect();
        c.apply(&tree).unwrap();
        assert!(matches!(c.list("User:anne", "read", "Folder", 1000).unwrap(), Listed::TooMany));
        assert_eq!(c.list_all("User:anne", "read", "Folder").unwrap().len(), 1501, "every folder under her grant, for the index");
        let (feed, mut reports) = tokio::sync::mpsc::unbounded_channel();
        let c = std::sync::Arc::new(c);
        let following = c.clone();
        tokio::spawn(async move { following.follow(feed).await });
        assert_eq!(reports.recv().await, Some(crate::engine::Feed::Gap), "what changed before it started is not known");
        c.apply(&[edge("Folder:2000", "Folder:0")]).unwrap();
        assert_eq!(reports.recv().await, Some(crate::engine::Feed::Changed(None)));
        // the same parents read again: no change
        let rows: Vec<(String, String, String)> =
            tree.iter().chain([&edge("Folder:2000", "Folder:0")]).map(|e| (e.object.clone(), "in".into(), e.subject.clone())).collect();
        assert!(c.replace(&rows, c.version()).unwrap());
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), reports.recv()).await.is_err());
        // policies that do not grant by id cannot be listed
        let attrs = load(
            "entity User; entity Doc { owner: User }; action read appliesTo { principal: User, resource: Doc };",
            r"permit (principal, action, resource) when { resource.owner == principal };",
        );
        assert!(attrs.list_all("User:anne", "read", "Doc").unwrap_err().contains("do not name its objects by id"));
    }

    #[test]
    fn an_action_requiring_a_context_is_refused() {
        let schema = "entity User; entity Doc; action view appliesTo { principal: User, resource: Doc, context: { ip: String } };";
        let refused = compile(BTreeMap::from([("schema".to_owned(), schema.to_owned())])).err().unwrap_or_default();
        assert!(refused.contains("requires a context"), "{refused}");
        let optional = "entity User; entity Doc; action view appliesTo { principal: User, resource: Doc, context: { ip?: String } };";
        assert!(compile(BTreeMap::from([("schema".to_owned(), optional.to_owned())])).is_ok());
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// `cargo test --release timing -- --ignored --nocapture`: a check of 1000 rows for a principal
    /// in 50 groups.
    #[test]
    #[ignore = "a timing, not a test"]
    fn check_a_thousand_rows() {
        let c = Cedar::new();
        let schema = "entity Group; entity User in [Group]; entity Doc { owner: User, org: Group, size: Long };
                      action view appliesTo { principal: User, resource: Doc };";
        let policies = r"permit (principal, action, resource) when { resource.owner == principal };
                          permit (principal, action, resource) when { principal in resource.org && resource.size < 10 };";
        c.load(BTreeMap::from([("schema".to_owned(), schema.to_owned()), ("p".to_owned(), policies.to_owned())])).unwrap();
        let edges: Vec<Change> = (0..50)
            .map(|g| Change {
                write: true,
                object: "User:anne".into(),
                relation: "in".into(),
                subject: format!("Group:g{g}"),
                ..Change::default()
            })
            .collect();
        c.apply(&edges).unwrap();
        let ids: Vec<String> = (0..1000).map(|i| i.to_string()).collect();
        let refs: Vec<&String> = ids.iter().collect();
        let rows: Vec<Value> = (0..1000).map(|i| json!({"owner": "bob", "org": format!("g{}", i % 80), "size": i % 20})).collect();
        let attrs: Vec<Option<&Value>> = rows.iter().map(Some).collect();
        let started = std::time::Instant::now();
        let allowed = c.check("User:anne", "view", "Doc", &refs, &attrs, Some(&json!({"roles": [], "attr": {}}))).unwrap();
        println!("1000 rows in {:?}, {} allowed", started.elapsed(), allowed.iter().filter(|a| **a).count());
    }
}
