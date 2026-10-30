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
   ([measured under load](https://github.com/filipecabaco/pg_auth/blob/main/docs/performance.md#under-load-and-with-the-sidecar-on-another-host)). Give the sidecar's own connection to Postgres
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

The setup is two things, both kept in Postgres with `authz.set_rules`, usually in a migration:

- **The rules**, in the engine's own language: an OpenFGA model, a SpiceDB schema, Cerbos
  policies, a Cedar schema and policies. pg_auth does not change them; the sidecar pushes them to
  the engine.
- **The mapping**, in YAML: which tables hold which of the rules' types, what reading and writing
  a row asks, and which tables' rows are relationships. This is the only part pg_auth reads, and
  it is the same shape for every engine.

```sql
select authz.set_rules(engine, name, rules, mapping => yaml);
-- the same call, the mapping first (named arguments go in any order)
select authz.set_rules(engine, name, mapping => yaml, body => rules);
```

When the transaction commits, each table the mapping names gets plain RLS policies that ask
pg_auth (`authz.permits('document', 'viewer', id)`, listed in `pg_policies`), and each
relationship table a trigger that delivers its rows to the engine. The policies hold no rule of
their own: the engine decides every row (`compare/demo.exs` changes the engine behind Postgres's
back and the rows follow).

The four examples below are the home page's demo (`docker/demo/rules` and
`docker/demo/mappings`): documents belong to an org, a membership makes a user an admin or a
member of it, and a share gives one user one document, until a date.

### OpenFGA

The model's types and relations are what the mapping names. A table's type defaults to its name
(`documents` is `document`), and a relationship's subject type comes from the relation (`org`
takes `[org]`), so the mapping only says what the table's name does not.

```sql
-- who is asking: the token's sub
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
# an org's members (org:<org_id>#<role>@user:<user_id>); only admins change them
memberships:
  type: org
  id: org_id
  read: can_read
  write: can_manage
  rows: {object: org_id, relation: role, subject: user_id}
# type document, its id the primary key; a new document is checked on its org
documents:
  read: viewer
  write: editor
  insert: org#can_manage from org_id
  relations:
    org: org_id
# document:<doc_id>#shared_with@user:<user_id>, until expires_at
shares:
  type: document
  id: doc_id
  read: viewer
  write: editor
  relations:
    shared_with: {subject: user_id, until: expires_at}
$yaml$);
```

A membership inserted through PostgREST then shows its org's documents to the new member, and a
share shows until its `expires_at`. Rows write relations (`admin`, `shared_with`); tables are
read by permissions computed from them (`viewer`). A role column naming a relation the model does
not have fails the insert itself (`check_violation`).

### SpiceDB

The same mapping, word for word, with the schema in SpiceDB's language. A relationship that ends
(`until`) needs a relation that takes an expiration (`use expiration`, `user with expiration`):
SpiceDB then ends it itself, even with the sidecar down.

```sql
select authz.set_rules('spicedb', 'schema', $zed$
use expiration

definition user {}

definition org {
  relation admin: user
  relation member: user
  permission can_read = member + admin
  permission can_manage = admin
}

definition document {
  relation org: org
  relation shared_with: user | user with expiration
  permission viewer = shared_with + org->can_read
  permission editor = org->can_manage
}
$zed$, mapping => $yaml$
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

SpiceDB only takes writes to relations, never to permissions; the mapping refuses a permission
as a relation when the rules are stored.

### Cerbos

Cerbos decides on the row's own fields, so there are no relationship tables: the mapping names the
table a resource policy covers, the actions reading and writing a row ask, and the columns sent as
the resource's attributes. The type is the policy's `resource:` (`document`, from `documents`).
Store one rule per policy, named as you would name its file.

```sql
-- who is asking: the token's sub, its role claim the principal's role
select authz.configure(subject => 'jwt');

select authz.set_rules('cerbos', 'resource/document', $cerbos$
apiVersion: api.cerbos.dev/v1
resourcePolicy:
  version: default
  resource: document
  rules:
    # anyone signed in reads published documents and their own drafts
    - actions: ["view"]
      effect: EFFECT_ALLOW
      roles: ["authenticated"]
      condition:
        match:
          any:
            of:
              - expr: request.resource.attr.owner == request.principal.id
              - expr: request.resource.attr.status == "published"
    # only the owner edits
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
  # sent as request.resource.attr.owner and .status
  attributes: [owner, status]
$yaml$);
```

The policy's plan for the subject becomes a filter on those columns (`owner = 'anne' or status =
'published'`), which indexes serve as they serve RLS. A column named differently from the
attribute is `owner_id as owner`. Cerbos needs its Admin API on to take pushed policies
([Versions, pushes and checks](guides/migrations.md#versions-pushes-and-checks)).

### Cedar

Cedar runs inside the sidecar: no engine server. The rule named `schema` is Cedar's schema and
carries the mapping; any other rule holds policies. Types are the schema's entity types
(capitalised: `Document` from `documents`), and a relationship is Cedar's `in`: the row puts its
object in its subject, the parent the policies test with `principal in ...`. Attributes are the
entity's, read by the policies as `resource.org`.

```sql
-- who is asking: the token's sub, a Cedar User
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
# a membership puts its user in the org, and in OrgAdmins when its role is admin
memberships:
  - {type: Org, id: org_id, read: read}
  - {type: OrgAdmins, id: org_id, read: manage, write: manage}
  - type: User
    relations:
      in: {object: user_id, subject: org_id, subject_type: Org}
  - type: User
    relations:
      in:
        object: user_id
        subject: org_id
        subject_type: OrgAdmins
        when: "role = 'admin'"
documents:
  read: view
  write: edit
  attributes: [org_id as org, org_id as admins]
# a share puts its user in the document, until expires_at
shares:
  - type: Document
    id: doc_id
    read: view
    write: edit
    attributes: [org_id as org, org_id as admins]
  - type: User
    relations:
      in:
        object: user_id
        subject: doc_id
        subject_type: Document
        until: expires_at
$yaml$);

select authz.set_rules('cedar', 'documents', $cedar$
// an org's members read its memberships; its admins change them
permit (principal, action == Action::"read", resource is Org)
  when { principal in resource };
permit (principal, action == Action::"manage", resource is OrgAdmins)
  when { principal in resource };
// a document is read by its org's members and by whoever it is shared with
permit (principal, action == Action::"view", resource is Document)
  when { principal in resource.org };
permit (principal, action == Action::"view", resource is Document)
  when { principal in resource };
// and written, or shared, by its org's admins
permit (principal, action == Action::"edit", resource is Document)
  when { principal in resource.admins };
$cedar$);
```

A table protected twice (`memberships` as `Org` and as `OrgAdmins`) is a list of entries. Partial
evaluation turns the policies into filters on the columns: `principal in resource.org` becomes
`org_id = ANY(the subject's orgs)`. A share carries its document's org, which the policy trusts to
decide who may share, so it is held to the document by a key (`foreign key (doc_id, org_id)
references documents (id, org_id)`) and filled in by a trigger (`docker/demo/sql/cedar.sql`).

Rules managed in the engine itself, not stored by pg_auth, take a mapping alone:
[Rules managed in the engine itself](#4-rules-managed-in-the-engine-itself).

### Going further

- [Mappings](guides/mappings.md): every key, relationship tables, conditions and ends, and what
  each error means.
- [Migrations](guides/migrations.md): storing and changing rules and mappings over a project's
  life, versions and pushes, the first admin, dry runs and checks after deploy.
- [Debugging](guides/debugging.md): a user who sees nothing, a refused write, failing queries, a
  grant that does not show.

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
  rows: {object: org_id, relation: role, allowed: [admin, member], subject: user_id, subject_type: user}
$yaml$);
```

pg_auth cannot read rules it does not keep, so the mapping names every type (`type:`), every
relationship's subject type (`subject_type:`), and the relations a row may name (`allowed:`, else
any, for the engine to refuse after the commit); the commit-time check against the rules is the
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
