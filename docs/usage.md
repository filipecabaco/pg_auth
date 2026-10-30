# Using pg_auth

pg_auth enforces an external authorization engine's decisions inside Postgres, as row level
security, for every client: PostgREST, application servers, ad hoc SQL. The engine decides; a
sidecar asks it (OpenFGA, SpiceDB or Cerbos, behind the same protocol, or runs Cedar itself),
beside Postgres or on another host; pg_auth's
planner hook shapes each query so that checking is cheap. Postgres 15 to 18. How it works:
[design.md](design.md).

## 1. Install

pg_auth runs where you configure Postgres yourself: a VM, Docker, Kubernetes, self-hosted
Supabase. It does not run on RDS, Aurora, Cloud SQL, Neon or hosted Supabase: they do not let you
preload an extension, and running the sidecar elsewhere over HTTPS does not change that.

1. **Build the extension.** It needs Rust (`mise install` pins the version), `cargo-pgrx` 0.19.3
   (`cargo install cargo-pgrx --version 0.19.3 --locked`, then
   `cargo pgrx init --pg17 "$(which pg_config)"`), clang, and the Postgres server headers
   (`postgresql-server-dev-17` on Debian or Ubuntu). Then
   `cargo pgrx install --release --pg-config "$(which pg_config)"`, with `--sudo` when Postgres's
   directories belong to root. Or build an image: `bench/docker/Dockerfile` is the official
   Postgres image with pg_auth preloaded (`--build-arg PG_MAJOR=17`).
2. **Preload it.** `shared_preload_libraries = 'pg_auth'` in `postgresql.conf`, restart Postgres,
   then `create extension pg_auth` as a superuser.
3. **Run an engine.** OpenFGA needs a store (`curl -X POST "$OPENFGA_URL/stores" -d
   '{"name": "app"}'`; its `id` is `OPENFGA_STORE_ID`) and a datastore that persists. SpiceDB needs
   its HTTP gateway on, and `track_commit_timestamp` on in its Postgres datastore for the change
   feed. Cerbos needs its storage in a database with its Admin API on, to take the rules from
   Postgres. Cedar needs none: the sidecar runs it, its policies and parents kept in Postgres.
4. **Run the sidecar** (`sidecar/query`, see its
   [README](https://github.com/filipecabaco/pg_auth/blob/main/sidecar/query/README.md)) pointed at
   the engine: `OPENFGA_URL` and `OPENFGA_STORE_ID`, `SPICEDB_URL` and `SPICEDB_TOKEN`, or
   `CERBOS_URL` with `CERBOS_ADMIN_USER` and `CERBOS_ADMIN_PASSWORD`, or `PG_AUTH_ENGINE=cedar`. Give it
   `PG_AUTH_DATABASE_URL`, a login holding `authz_sidecar` (below): through it the sidecar pushes
   the rules stored with `authz.set_rules`, delivers relationship tables' rows, and keeps the
   permission index. Without it, none of the three happens.
5. **Point Postgres at it**, superuser-only since its answers decide what every query reads. Beside
   Postgres, on a Unix socket: the sidecar's `PG_AUTH_SOCKET` and Postgres's
   `pg_auth.sidecar_socket` name the same path (both default to `/tmp/pg_auth.sock`; the sidecar's
   image uses `/run/pg_auth/pg_auth.sock`), and the sidecar runs in the Postgres OS user's group,
   since the socket is created 0660:
   `alter system set pg_auth.sidecar_socket = '/run/pg_auth/pg_auth.sock'`.
6. **Or on another host, over HTTPS:** `PG_AUTH_LISTEN`, a secret and a certificate on the sidecar,
   `pg_auth.sidecar_url` and `pg_auth.sidecar_secret_file` in Postgres. Requests and answers are
   signed with the secret, and anything else is refused; a round trip costs about 0.2 ms more than
   the socket, plus the network's, paid once by a request that asks the sidecar
   ([measured under load](performance.md#under-load-and-with-the-sidecar-on-another-host)). Give the sidecar's own connection to Postgres
   `?sslmode=verify-full&sslrootcert=...` in `PG_AUTH_DATABASE_URL` (libpq's modes), and keep both
   hosts' clocks within 30 s of each other. See the sidecar's
   [README](https://github.com/filipecabaco/pg_auth/blob/main/sidecar/query/README.md#over-the-network);
   `docker/remote` runs it with `docker compose`.

`select * from authz.status()` shows the engine, the socket, the subject source and every
protected table.

### Who may use what

pg_auth keeps its objects in three schemas, one per kind of user, and makes two roles to grant
them (roles belong to the cluster: a second database finds them, and `drop extension` leaves them):

| schema | holds | reached by |
|---|---|---|
| `authz` | what queries call (`permits` and the helpers the planner writes into them), and what people call: `set_rules`, `configure`, `why`, `doctor`, `status`, `check`, `lint`, `can`, `current_subject`, `explain` | every role; its admin functions only by `authz_admin` |
| `authz_sidecar` | the sidecar's functions: claiming relationship changes, pushing rules, filling the permission index | `authz_sidecar` |
| `authz_internal` | the tables, and what a mapping compiles into (internal: `set_rules` calls it as the migrating role, which must own the tables it protects) | `authz_admin` |

```sql
grant authz_admin to migrations;                                       -- stores the rules, configures pg_auth
create role sidecar login password '...' in role authz_sidecar;        -- PG_AUTH_DATABASE_URL's login
```

A function added to `authz_internal` or `authz_sidecar` is closed until a role is granted it. The
admin functions run as their caller, so protecting a table still needs owning it: `authz_admin`
cannot add policies to someone else's table. It writes the configuration tables, and only reads
the permission index, the outbox and the feed's position, which pg_auth's own functions write.

### On self-hosted Supabase

Not tested yet on the Supabase stack; what applies from how Supabase sets up Postgres:

- The `supabase/postgres` image does not include pg_auth: build it against that image's
  `pg_config` and add it to its `shared_preload_libraries`.
- `create extension pg_auth` and `alter system` need a superuser (`supabase_admin`): `postgres` is
  not one there. Protecting a table needs owning it, so a migration run as `postgres` cannot
  protect `storage.objects`, which `supabase_storage_admin` owns.
- `authz.configure(subject => 'jwt')` reads `sub` from the claims PostgREST sets. To verify the
  token's signature instead, give `jwt_signed` the project's JWT secret as an `oct` key:
  `{"keys": [{"kty": "oct", "k": "<the secret, base64url>"}]}`.
- `anon` requests carry no `sub`: they see no protected rows. `service_role` bypasses row level
  security, and so pg_auth.
- Realtime and Storage read through Postgres, so pg_auth's policies apply to them, but neither is
  tested yet; Realtime checks each change once per subscriber, a sidecar question each.

## 2. Who is asking

```sql
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.configure(subject => 'jwt_signed', jwks => '{"keys": [...]}');  -- verify the token
select authz.configure(subject => 'guc');      -- pg_auth.subject_id, set by a trusted role
```

- `jwt` reads a claim from the request's claims (Supabase/PostgREST set `request.jwt.claims`
  per request); `claim` may be a dotted path (`app_metadata.user_id`). The claims are not
  signature-checked: use it where only a trusted API can set them.
- `jwt_signed` verifies the bearer token from `request.headers` against the JWKS.
- `guc` reads `pg_auth.subject_id` (and `pg_auth.subject_type`, default `user`), which only
  superusers and roles granted `SET ON PARAMETER pg_auth.subject_id` can set.

No subject means no rows. `select * from authz.current_subject()` says who pg_auth sees and
why.

## 3. The rules and their mapping

The setup is the engine's rules, in the engine's own syntax, and the mapping of the tables they
decide on, both kept in Postgres with `authz.set_rules`. The mapping says, in YAML, which tables
hold which types, what reading and writing a row asks, and which tables' rows are relationships;
the rules go to the engine as they are. This is the whole setup of the demo on the home page, on
OpenFGA:

```sql
select authz.configure(subject => 'jwt');

select authz.set_rules('openfga', 'model', $fga$
model
  schema 1.1
type user
type org
  relations
    define admin: [user]
    define member: [user]
    define can_read: member or admin
    define can_manage: admin
type document
  relations
    define org: [org]
    define shared_with: [user]
    define viewer: shared_with or can_read from org
    define editor: can_manage from org
$fga$, mapping => $yaml$
memberships:
  type: org
  id: org_id
  read: can_read
  write: can_manage
  rows: {object: org_id, relation: role, subject: user_id}
documents:
  read: viewer
  write: editor
  insert: org#can_manage from org_id
  relations:
    org: org_id
shares:
  type: document
  id: doc_id
  read: viewer
  write: editor
  relations:
    shared_with: {subject: user_id, until: expires_at}
$yaml$);
```

When the transaction commits, the three tables are protected, the rows of `memberships`,
`documents` and `shares` are relationships in OpenFGA, and the sidecar sends it the model. A
membership inserted through PostgREST then shows its org's documents to the new member, and a
share shows until its `expires_at`.

The mapping is the same for every engine. On SpiceDB it is this one, word for word, with the
schema in SpiceDB's syntax (`docker/demo/mappings/spicedb.yaml`). An attribute policy on Cerbos
maps its table and the attributes it reads:

```sql
select authz.set_rules('cerbos', 'resource/document', $cerbos$
apiVersion: api.cerbos.dev/v1
resourcePolicy:
  version: default
  resource: document
  rules:
    - actions: ["view"]
      effect: EFFECT_ALLOW
      roles: ["authenticated"]
      condition:
        match:
          any:
            of:
              - expr: request.resource.attr.owner == request.principal.id
              - expr: request.resource.attr.status == "published"
    - actions: ["edit"]
      effect: EFFECT_ALLOW
      roles: ["authenticated"]
      condition:
        match:
          expr: request.resource.attr.owner == request.principal.id
$cerbos$, mapping => $yaml$
documents:
  read: view
  write: edit
  attributes: [owner, status]
$yaml$);
```

On Cedar, the rule named `schema` is the schema and carries the mapping; every other rule holds
policies. A relationship is the relation `in`: the object is in the subject. The subject is a
`User`, since Cedar's types are capitalised, and a share carries its document's org, which the
policy reads (the demo fills it in with a trigger, `docker/demo/sql/cedar.sql`):

```sql
select authz.configure(subject => 'jwt', subject_type => 'User');

select authz.set_rules('cedar', 'schema', $cedar$
entity Org;
entity OrgAdmins;
entity User in [Org, OrgAdmins, Document];
entity Document { org: Org, admins: OrgAdmins };
action read appliesTo { principal: User, resource: Org };
action manage appliesTo { principal: User, resource: OrgAdmins };
action view, edit appliesTo { principal: User, resource: Document };
$cedar$, mapping => $yaml$
memberships:
  - {type: Org, id: org_id, read: read}
  - {type: OrgAdmins, id: org_id, read: manage, write: manage}
  - type: User
    relations:
      in: {object: user_id, subject: org_id, subject_type: Org}
  - type: User
    relations:
      in: {object: user_id, subject: org_id, subject_type: OrgAdmins, when: "role = 'admin'"}
documents:
  read: view
  write: edit
  attributes: [org_id as org, org_id as admins]
shares:
  - {type: Document, id: doc_id, read: view, write: edit, attributes: [org_id as org, org_id as admins]}
  - type: User
    relations:
      in: {object: user_id, subject: doc_id, subject_type: Document, until: expires_at}
$yaml$);

select authz.set_rules('cedar', 'documents', $cedar$
permit (principal, action == Action::"read", resource is Org) when { principal in resource };
permit (principal, action == Action::"manage", resource is OrgAdmins) when { principal in resource };
permit (principal, action == Action::"view", resource is Document) when { principal in resource.org };
permit (principal, action == Action::"view", resource is Document) when { principal in resource };
permit (principal, action == Action::"edit", resource is Document) when { principal in resource.admins };
$cedar$);
```

Each protected table gets plain RLS policies that only ask pg_auth
(`authz.permits('document', 'viewer', id)`), so `pg_policies` lists them; pg_auth's planner hook
turns them into an indexable filter or a batched check. The policies hold no rule of their own:
the engine decides every row (`compare/demo.exs` changes the engine behind Postgres's back and
the rows follow).

**Rules kept in the engine itself** (Cerbos Hub, a model managed elsewhere) are stored without a
body: `authz.set_rules('openfga', 'model', null, mapping => ...)` sets up the tables and pushes
nothing. pg_auth cannot read such rules, so the mapping names every type and every subject type
(`type:`, `subject_type:`).

### The mapping

```yaml
<table>:                         # one entry, or a list of them (a table protected or related twice)
  type: <type>                   # default: the table's name, singular or plural, when the rules have it
  id: <column or expression>     # default: its primary key
  read: <permission>             # what reading a row asks; a protected table needs it
  write: <permission>            # what inserting, updating and deleting asks; without it, denied
  update: <permission>           # and delete: their own permission
  insert: <type>#<permission> from <column>   # inserts checked on a parent
  attributes: [<column>, <column> as <name>]  # the values the rules read
  tree: <table>(<id>, <parent>) from <permission>   # a permission inherited down a tree
  relations:                     # rows that are relationships of this type
    <relation>: <subject column>
    <relation>: {subject: <column>, subject_type: <type>, until: <column>, when: <sql>, ...}
  rows: {object: <column>, relation: <column>, subject: <column>}   # the relation in a column
```

A key it does not know, a type, relation or permission the rules do not have, or a column the
table does not have fails when the transaction commits, naming the table and what the rules have.

- **The id** is a column (the row's own id, or a parent's such as `org_id`), or an IMMUTABLE
  expression over the row (`id: "split_part(name, '/', 1)"`, a storage path's first folder).
  Index it as you would for RLS. Partitions created later are protected too.
- **Several ways to reach a row** (an order visible to its buyer, or to its seller's staff): a
  list of entries, one per way, each with its type. A row is visible when any allows it.
- **An insert checked on a parent:** a table protected by its own id cannot authorize an INSERT,
  the engine has no object yet (`insert: project#create from project_id`).
- **A tree** (folders with `parent_id`): `tree: folders(id, parent_id) from viewer_direct`, with the
  model reading `viewer: viewer_direct or viewer from parent`. The engine lists only direct grants
  and Postgres expands them; triggers keep a closure of the tree.
- **Rules on the row's own attributes** (Cerbos, Cedar): `attributes: [owner_id, status]` sends
  those columns as the resource's attributes, `owner_id as ownerId` under another name. The
  engine's plan for the subject (always, never, or a condition on the attributes) is compiled into
  a filter on those columns: indexes serve it as they serve RLS, and a cached plan serves every
  subject whose plan has the same shape. A condition the engine expresses with more than
  comparisons, `and`, `or`, `not` and `in` is evaluated per row. The principal's roles come from
  the claim `roles_claim` (default `role`; `authz.configure(roles_claim => 'app_metadata.roles')`)
  or, with `subject => 'guc'`, from `pg_auth.subject_roles` (comma separated, superuser-only like
  `pg_auth.subject_id`); its attributes are the token's claims. **Scoped policies** (Cerbos: a
  tenant's overrides, `scope:` in the policy) need the request's scope:
  `authz.configure(scope_claim => 'tenant_id')` sends that claim as Cerbos's scope
  (`pg_auth.subject_scope` for a session subject; `policy_version` for a version other than
  `default`). Without one, a scoped override does not apply; `authz.doctor()` says so.
- **Row state** (`not deleted`, `published`) stays plain SQL: add an ordinary restrictive policy
  next to pg_auth's; it joins the scan's filter.

### Rows that are relationships

An app with no backend changes who may see what by writing rows, through PostgREST like any other
table: a membership, a share, a team. The mapping makes a table's rows relationships in the
engine (OpenFGA, SpiceDB, or Cedar's parents), and pg_auth delivers each change:

```yaml
memberships:
  type: org
  rows: {object: org_id, relation: role, subject: user_id}   # org:<org_id>#<role>@user:<user_id>
documents:
  relations:
    org: org_id                                               # document:<id>#org@org:<org_id>
shares:
  type: document
  relations:
    shared_with: {object: doc_id, subject: user_id}           # document:<doc_id>#shared_with@user:<user_id>
```

- `rows` reads the relation from a column. The relations a row may name are the ones in the rules
  that take the subject; a row naming another fails at once (`check_violation`), where the app sees
  it, instead of the engine refusing it after the commit.
- `relations` writes one relation per entry. The object is the entry's `id`, or `object`, else the
  table's single-column primary key.
- The subject's type comes from the rules (the relation's one directly related type). A
  relation's keys say otherwise:

| key | |
|---|---|
| `subject` | the subject's column |
| `subject_type`, `subject_relation` | the subject's type, or a userset (`subject_type: team, subject_relation: member`: the team's members) |
| `subject_id` | a fixed subject, `'*'` for everyone (`user:*`): no subject column |
| `object` | the object's column, when not the entry's id |
| `until` | the relationship ends at the row's timestamp (null: for good) |
| `condition` | written with an OpenFGA condition or SpiceDB caveat, the row giving its parameters (`{name: in_region, with: {allowed: regions}}`) |
| `when` | only rows matching this SQL (one `shares` table for users, teams and the public, one entry per kind) |
| `name` | the declaration's name, when the generated `<type>_<relation>` is not wanted |

- **Write to relations, read permissions.** The rows write plain relations (`admin`, `member`,
  `shared_with`); tables are protected by permissions computed from them (`viewer: shared_with or
  can_read from org`). SpiceDB only takes writes to relations, and the model reads the same in
  OpenFGA.
- **When it shows.** A trigger records each insert, delete and relationship-changing update in
  `authz_internal.outbox` with the write itself, so a rolled-back write never reaches the engine. The
  sidecar delivers it on commit, and every backend asks again: the change shows within tens of
  milliseconds (`compare/relationships.exs`).
- **Grants that end.** `until expires_at`: the sidecar deletes the relationship from the engine
  when it is due, and every backend asks again, as for any revocation: it goes within a few
  hundred milliseconds of its end. An end moved later keeps it, one moved earlier or into the past
  ends it now, a renewal writes it again; a row whose end has passed is no relationship at all.
  SpiceDB also gets the end as the relationship's expiration, so the engine ends it itself if the
  sidecar is down (within its revision window, a few seconds). Its schema says so: `use
  expiration`, and `relation shared_with: user | user with expiration` (`user with expiration`
  alone when the column is `not null`). OpenFGA has no expiry of its own: its grants end when the
  sidecar deletes them, so while the sidecar is down an ended grant stays, like any revocation.
  Ends waiting in the outbox are not changes waiting: `authz.doctor()` counts them apart.
- **Conditions and caveats.** A relationship can carry an OpenFGA condition or a SpiceDB caveat,
  the row giving its values, and each request the rest: a share in some regions, a grant until a
  deadline, an office's IP range.

  ```text
  condition in_region(region: string, allowed: list<string>) {     # OpenFGA
    region in allowed
  }
  type doc
    relations
      define viewer: [user, user with in_region]
  ```

  ```yaml
  shares:
    type: doc
    relations:
      viewer: {object: doc_id, subject: user_id, condition: {name: in_region, with: {allowed: regions}}}
  ```

  ```sql
  -- what every request gives: the time asked, the token's region claim, a header
  select authz.configure(context => '{"current_time": "now", "region": {"claim": "region"},
                                      "client_ip": {"setting": "request.headers", "key": "x-forwarded-for"}}');
  ```

  - A null column gives no value. New values in the row write the relationship again with them.
  - `authz.configure(context => ...)` names what every request gives: `"now"` (the time the
    engine is asked, RFC 3339), `{"claim": "region"}` (the token's claim, `a.b` for a nested one),
    or `{"setting": "request.headers", "key": "x-forwarded-for"}` (a setting, or a key of its
    JSON). A value the request lacks is not sent, and a condition needing it is not met: those
    rows stay hidden.
  - Answers depend on the context, so they are kept per context; one asked with the time is kept
    up to the sidecar's `CACHE_TTL_MS` (1 s), so a grant ends within that of its deadline. The
    permission index is not used while a context is configured: a set is filled for no request
    in particular. Broad subjects are then checked row by row.
  - **The protected row's own values.** A condition can also take values from the row being read
    (a report's level against a reader's clearance): protect the table with them as attributes,
    named as the condition's parameters (`reports: {read: viewer, attributes: [level]}`). What that costs depends on the engine, since an engine's list cannot take a value
    per row:

    | engine | how rows are decided | expect |
    |---|---|---|
    | SpiceDB | one LookupResources per subject (with the request's context): rows granted outright and rows not granted at all are decided in Postgres, by an indexable `id = ANY(...)` filter; only rows granted through a caveat that needs the row's values are checked, each with them | the cost of a list, plus one round trip per row whose grant is conditional. A subject with more than `LIST_LIMIT` objects has every row checked |
    | OpenFGA | ListObjects takes no row's values (and fails on a condition missing one), so every row read is checked with its values | one round trip per row read: fine for reads by key and small pages, slow for scans. Prefer SpiceDB for this shape, or a column the relationship can carry instead |
    | Cerbos | attributes are its own model: plans compile into filters on the columns | no round trip per row |
    | Cedar | partial evaluation leaves conditions on the row: compiled into filters on the columns (an entity attribute among the subject's ancestors becomes `org_id = ANY(...)`, `resource in` the ids below an entity); a part outside comparisons, `in`, `has`, `&&`, `\|\|` and `!` is decided by Cedar for the rows the rest leaves | no round trip per row; the sidecar evaluates a row itself |
- **If the engine is down,** changes wait and are delivered when it is back. A change the engine
  refuses is set aside with its reason, and `authz.doctor()` reports it; the changes after it go
  on.
- **Needs** the sidecar's `PG_AUTH_DATABASE_URL` (as for the permission index), and a unique key
  over the columns, so two rows are never one relationship. Rows already in the table are
  delivered when the mapping first names the table.

### Versions, pushes and checks

```sql
select engine, name, version, mapping, pushed_at, error from authz.rules();   -- the versions in force
select jsonb_pretty(authz_internal.setup());                                  -- what the mappings set up
```

- **Versions.** Every change is a new version in `authz_internal.rules`, with who stored it and when; the
  same rules and mapping again are not one. `authz.rules()` shows the latest of each, `body` and
  `mapping` included. A migration carries the rules and their mapping with the tables they
  protect, and they are reviewed as written.
- **What changes.** A new version sets up what its mapping says: a table it no longer names loses
  its protection or stops being a relationship table, one unchanged is left as it is (its rows are
  not delivered again). A version changing only the mapping is not pushed again: the engine has
  its rules already. Rules stored with no mapping keep the one before, so updating the rules alone
  leaves the tables as they are; `mapping => ''` removes it.
- **Pushed.** The sidecar pushes the latest version within milliseconds (OpenFGA: a new
  authorization model, which it then reads with; SpiceDB: WriteSchema; Cerbos: its Admin API), and
  every backend asks again, since new rules may change any answer. Each push is recorded in
  `pushed_at`. Relationships wait for the rules they need: none is delivered while that engine's
  rules are not pushed.
- **Checked.** The mapping is applied when the rules are stored, so its tables and columns must
  exist by then, and a mapping that is not YAML, or names a key it does not take, fails at once,
  naming its table. A mapping naming a type or permission the rules do not have fails when the
  transaction commits, naming what they have:

  ```text
  ERROR:  the setup names what the rules stored with authz.set_rules do not have:
  public.docs: document has no vewer in the openfga model (it has: editor, org, viewer)
  ```

  That check runs at commit, so a migration can change the rules and the tables using them
  together. A relationship that ends needs, on SpiceDB, a relation that takes it with an
  expiration (and as it is, when the column may be null). One written with a condition needs a
  relation that takes `user with <condition>`, a condition the rules have, and parameters it
  takes. Cerbos policies are checked for their resource and actions (`approve:*` covers
  `approve:big`). What the stored rules do not show, the engine still checks when they are pushed.
- **Refused.** Rules the engine refuses (an undefined relation, a policy that does not compile) are
  kept with its reason in `error`, `authz.doctor()` fails its `rules` check, and the engine keeps
  the version before. Cerbos stores policies it cannot compile and goes on with the ones before:
  the sidecar asks it whether they compile, and puts the versions before back when they do not.
  Store a corrected version to replace it.
- **OpenFGA's DSL** is converted to the API's JSON by the sidecar (schema 1.1: types, relations,
  `[user, user:*, team#member, user with in_region]`, `or`, `and`, `but not`, `from`, conditions);
  modules are refused. JSON is taken as is.
- **Every sidecar follows them.** The engine's id for the rules in force (OpenFGA's model, SpiceDB's
  schema revision) is kept beside them, and every sidecar asks with it from the next request, not
  only the one that pushed them.
- **Cedar** is pushed to no server: the sidecar holding the push compiles the rules in force with the
  new version and validates the policies against the schema (strictly); a version that does not
  is refused with Cedar's reason, the version before kept. Every sidecar then loads the rules in
  force, and the parents delivered (kept in `authz_internal.entity_parents`, read again after each
  delivery). Not taken: a schema with a namespace (a subject `type:id` cannot name `App::User`),
  policy templates, and the request's context.
- **Cerbos** must store policies in a database with its Admin API on
  (`storage.driver: sqlite3` or `postgres`, `server.adminAPI.enabled: true`), and the sidecar needs
  its credentials (`CERBOS_ADMIN_USER`, `CERBOS_ADMIN_PASSWORD`). One `authz.set_rules` row per
  policy, named as you would name its file.

## 4. Rules managed in the engine itself

Rules kept in the engine, or in a policy directory or git store Cerbos reads, stay there: pg_auth
pushes only what `authz.set_rules` stored. Store their mapping alone, with no body:

```sql
select authz.set_rules('openfga', 'model', null, mapping => $yaml$
docs:
  type: doc
  id: id
  read: viewer
  write: editor
memberships:
  type: org
  rows: {object: org_id, relation: role, subject: user_id, subject_type: user}
$yaml$);
```

pg_auth cannot read rules it does not keep, so the mapping names every type (`type:`) and every
relationship's subject type (`subject_type:`), and the commit-time check against the rules is the
engine's to make when it is asked. Store the engine's rules before declaring tables that write
their relations: the engine refuses a relationship whose relation its model does not have yet.

## 5. Inspect and debug

```sql
select * from authz.doctor();         -- the setup checked, with the statement that fixes each problem
select * from authz.why('app.documents', '42', as_subject => 'user:anne', as_role => 'authenticated');
--  whether and why that row is visible: subject, row level security, privilege, each policy, verdict
select * from authz.why('app.documents', '42', as_subject => 'user:anne', subject_roles => array['editor']);
--  the same for an engine that decides on roles (Cerbos): the roles anne's token would carry.
--  A policy the engine cannot decide shows as a failed step with the engine's error
set local pg_auth.as_subject = 'user:anne';   -- see the data as anne does (superuser only)
select * from authz.explain('select * from app.documents order by created_at limit 20');
--  per protected table: mode (listed ids / batched check / ...), the reason, and advice
select * from authz.last_statement_advice();
--  for the last statement: ids sent to the engine, engine time, ids answered from the index
select * from authz.stats();          -- this backend's round trips, ids checked, engine time
select authz.can('viewer', 'document:42');                 -- the current subject
select authz.check('user:anne', 'viewer', 'document:42');  -- any subject (administrators)
select * from authz.lint();           -- hand-written RLS policies scored against common issues
```

`pg_auth.warn_ms` (off by default) raises a WARNING with the advice when a statement's engine
time reaches it, for clients that never call `last_statement_advice()`.

## 6. Settings

The ones that change how much the sidecar and the engine are asked are superuser-only, so a role
cannot turn them against the engine others share (`ALTER ROLE ... SET` by a superuser still
sets them per role).

| setting | default | what it does |
|---|---|---|
| `pg_auth.sidecar_socket` | `/tmp/pg_auth.sock` | the sidecar's Unix socket (superuser) |
| `pg_auth.sidecar_url` | | an `https://` URL: reach the sidecar over the network instead of the socket (superuser) |
| `pg_auth.sidecar_secret_file` | | the file holding the secret requests to the URL are signed with (superuser) |
| `pg_auth.sidecar_ca_file` | | PEM roots to check the URL's certificate against, instead of the public ones (superuser) |
| `pg_auth.sidecar_timeout_ms` | 5000 | fail the query when the sidecar takes longer (superuser) |
| `pg_auth.subject_id`, `pg_auth.subject_type` | | the subject for `subject => 'guc'` |
| `pg_auth.subject_roles` | | its roles, comma separated, for an engine that decides on attributes |
| `pg_auth.plan_lookup` | on | shape protected scans at planning; off checks every row through `authz.permits` (superuser) |
| `pg_auth.batch_check` | on | check rows in batches (AuthzBatch) where ids cannot be listed (superuser) |
| `pg_auth.permission_index` | on | answer broad subjects from the permission index, and ask the sidecar to fill it (superuser) |
| `pg_auth.index_list_limit` | 0 | list a broad subject whose fresh index set has at most this many objects (`col = ANY(...)`) instead of checking rows against it; worth it only where broad subjects mostly scan, since every statement builds the array |
| `pg_auth.fill_wait_ms`, `pg_auth.fill_wait_after` | 10000, 2000 | how long, and after how many engine-checked ids, a heavy statement waits for its subject's index set (superuser) |
| `pg_auth.replan_on_breadth` | on | rebuild this backend's cached plans for a subject much broader or narrower than they were built for |
| `pg_auth.warn_ms` | off | warn when a statement's engine time reaches this |

The sidecar's own settings (list limit, cache TTL, check concurrency) are in its
[README](https://github.com/filipecabaco/pg_auth/blob/main/sidecar/query/README.md).

## 7. What it costs

Per scenario numbers against RLS written the way Supabase recommends are in
[the benchmarks](benchmarks.md); correctness against the engine is checked by
[`compare/parity.exs`](https://github.com/filipecabaco/pg_auth/blob/main/compare/README.md). Writes and very broad subjects cost more than
narrow reads; `authz.explain` and `authz.last_statement_advice()` say when and why.

Relationship tables cost engine writes: every insert, delete and relationship-changing update of
a declared table becomes one write to the engine, batched by the sidecar (up to 100 tuples a call
on OpenFGA, 1000 on SpiceDB), and the answers every backend kept are dropped after each batch.
A table that changes thousands of rows a second puts that load on the engine; declare the tables
that grant access, not every table that has an owner column.

A backend's first request also pays one-time work: reading the configuration and hierarchy
declarations, preparing pg_auth's own queries, connecting to the sidecar. `authz.warm()` does
it ahead of time, and when the session already has a subject it also fetches that subject's
lists and plans for every protected table. Call it once per connection, for example as a pooler's
connect query. Calling it again is cheap. It returns one row per step:

```sql
select * from authz.warm();
--  configuration | 3 settings
--  hierarchies   | 1 declared
--  queries       | 9 of 9 prepared
--  sidecar       | connected: openfga
--  subject       | user:anne: 4 of 4 lists and plans fetched   (only with a subject)
```

## 8. How fresh answers are

An answer can come from five places. Each keeps it for a bounded time, and a grant or revocation
written through a relationship table clears every one of them:

| where | kept | cleared by | worst case for a change made in the engine itself |
|---|---|---|---|
| the statement and its transaction | one answer per subject, type and permission | the transaction's end; a change of the request's context | the transaction |
| the backend, across transactions | until the sidecar's cached answer reaches half its TTL (0.5 s) | a delivered relationship change, pushed rules | 0.5 s |
| the sidecar | `CACHE_TTL_MS` (1 s); answers used within `REFRESH_ACTIVE_MS` (30 s) are refreshed ahead | a delivered relationship change, pushed rules | 1 s |
| the permission index (broad subjects only) | until the engine's change feed reports any change | the change feed: OpenFGA's ReadChanges every `INDEX_POLL_MS` (0.5 s), SpiceDB's Watch | the feed's delay (0.2 to 0.4 s measured), then the engine's own reads |
| the engine | OpenFGA's check and list caches (10 s when on); SpiceDB's `minimize_latency` reads (a revision up to 5 s old) | after a write through pg_auth, reads skip them: OpenFGA asks with `HIGHER_CONSISTENCY` for 10 s, SpiceDB at least as fresh as the write's ZedToken | 10 s (OpenFGA), 5 s (SpiceDB, or `SPICEDB_CONSISTENCY=fully_consistent` for the latest) |

- **Written through pg_auth** (a relationship table, `authz.set_rules`): visible 4 to 34 ms after
  the commit in the measurements (`compare/relationships.exs`), on both engines.
- **Written in the engine directly** (its API, another service): up to the engine's own window
  plus the sidecar's second: about 11 s on OpenFGA with caches, 6 s on SpiceDB by default.
- **A grant with an end** (`until`): gone 60 to 120 ms after its end through the outbox;
  on SpiceDB the engine also ends it itself, within its revision window.
- **A request context with the time** (`"now"`): answers are kept up to the sidecar's 1 s, so a
  condition on the time holds up to a second past its boundary.
- **The engine down:** answers already kept are used for at most the times above, then statements
  fail. Subjects answered from the permission index keep their last fresh set, read from
  Postgres; a change the engine makes meanwhile reaches it when the feed is read again.
- **A sidecar restarting:** the change feed resumes from the position it had read to, so the
  permission index is kept when nothing changed, and a change made while no sidecar ran marks its
  sets stale as soon as one starts.
- **The sidecar down:** answers already kept are used for at most its 1 s, then every statement
  on a protected table fails, the permission index included: nothing in Postgres answers alone.

## 9. What it does not do yet

- **Upgrades.** There are no extension update scripts: a new version is `drop extension` and
  `create extension` again, which drops pg_auth's policies, so the rules are stored again after.
  They start with the first release: from then on each schema change ships an update script, and
  CI installs the last release, adds data, runs `alter extension pg_auth update` and compares
  every object with a fresh install.
- **Restoring next to another engine.** A logical dump carries pg_auth's configuration, its rules
  and the changes not yet delivered (`compare/dump.exs`); the permission index is made again. Restored
  next to the same engine it goes on as before. Next to an empty one, the rules and the
  relationships are recorded as already pushed, and nothing pushes them again yet.
- **Connecting to the socket** has no timeout of its own: it waits only while the sidecar's
  queue of connections is full. Once connected, a wait on the sidecar gives way to a cancel or a
  `statement_timeout` within about 100 ms, on the socket and over HTTPS.
- **Read replicas.** Not tested; a replica's permission index follows the primary's with the
  replay's lag.
- **The first admin.** A relationship table protected with `write` lets only those who hold it add
  rows, so the first member of a new organisation is written by a privileged path: a
  `security definer` function, or a role that bypasses row level security.
- **Memory.** Each backend keeps answers of its own: up to 10,000 lists, and a million ids of loaded
  index sets.
- **Hosted OpenFGA with OAuth** (Okta FGA): the sidecar takes a static token only.
- **Cedar.** No namespaces, templates or request context yet (an action requiring a context is
  refused). An entity's own attributes other than the principal's (from its claims) and the row's
  are unknown to it: a permit reading a parent's attributes grants nothing, and a forbid reading
  them denies every row it could apply to. A principal whose type may be `in` a `Role` has its
  roles only from its token, sent with plans and with checks of rows given their attributes: a
  table protected without attributes is answered with them unknown, so a policy on roles decides
  none of its rows (they are denied). A subject whose objects Cedar's policies name by id (`resource in principal`) past `LIST_LIMIT`
  gets a permission index set, filled from them and made stale by any change to the parents or
  the rules. A sidecar that did
  not deliver a change reads the parents again after the delivery, a few milliseconds behind the
  one that did. A protected table's attributes come from columns of the type Cedar declares (a
  Long from an integer, a String from text, an entity's id from any column, matched by its exact
  text); another is refused when the setup is committed.
