# Using pg_auth

pg_auth enforces an external authorization engine's decisions inside Postgres, as row level
security, for every client: PostgREST, application servers, ad hoc SQL. The engine decides; a
sidecar next to Postgres asks it (OpenFGA, SpiceDB or Cerbos, behind the same protocol); pg_auth's
planner hook shapes each query so that checking is cheap. Postgres 15 to 18. How it works:
[design.md](design.md).

## 1. Install

- Build and install the extension (pgrx; Postgres 15 to 18) and add it to
  `shared_preload_libraries = 'pg_auth'`, then `create extension pg_auth`.
- Run the sidecar (`sidecar/query`) on the same host, listening on a Unix socket Postgres can
  reach, pointed at OpenFGA (`OPENFGA_URL`, `OPENFGA_STORE_ID`) or SpiceDB (`SPICEDB_URL`,
  `SPICEDB_TOKEN`; its HTTP gateway on, and `track_commit_timestamp` on in its Postgres datastore
  for the permission index's change feed). See its
  [README](https://github.com/filipecabaco/pg_auth/blob/main/sidecar/query/README.md).
- Point Postgres at it (superuser only, since its answers decide what every query reads):
  `alter system set pg_auth.sidecar_socket = '/run/pg_auth/default.sock'`.
- Optional: give the sidecar `PG_AUTH_DATABASE_URL` to keep the permission index in Postgres
  (subjects with more objects than the engine lists are then answered from Postgres).

`select * from authz.status()` shows the engine, the socket, the subject source and every
protected table.

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

## 3. Protect tables

```sql
select authz.protect('app.documents', type => 'document', id => 'id', read => 'viewer', write => 'editor');
-- or positionally:
select authz.protect('app.documents', 'document', 'id',     'viewer', 'editor');
select authz.protect('app.comments',  'document', 'document_id', 'viewer');
```

Arguments: the table, the object type a row is (`type`), the column holding its id (`id`, the
row's own id or a parent's such as `org_id`), the permission to read (`read`) and to write
(`write`; writes are denied without it). The table gets RLS policies that pg_auth's planner
hook turns into an indexable filter or a batched check; partitions created later are protected
too. Index the id column, as you would for RLS.

- **Several ways to reach a row** (an order visible to its buyer, or to its seller's staff):
  protect the table once per way, each with its own `name`. A row is visible when any allows it.
- **Inserts checked on the parent.** A table protected by its own id cannot authorize an INSERT
  (the engine has no object yet): `insert_type => 'project', insert_id => 'project_id',
  insert_permission => 'create'`.
- **A key that is an expression** (a storage path's first folder): `id => null, key =>
  'split_part(name, ''/'', 1)'`. It must be IMMUTABLE; index the expression.
- **A permission inherited down a tree the app stores** (folders with `parent_id`):
  `hierarchy => 'app.folders(id, parent_id)', direct => 'viewer_direct'`, with the model
  reading `viewer: viewer_direct or viewer from parent`. The engine lists only direct grants and
  Postgres expands them; triggers keep a closure of the tree. `authz.drop_hierarchy(type,
  permission)` removes the declaration.
- **Rules on the row's own attributes** (an engine that decides on them: Cerbos): `attributes
  => array['owner_id', 'status']` sends those columns, by name, as the resource's attributes.
  The policy is then `authz.permits_resource`, which the planner replaces with the subject's plan
  (always, never, or a condition on the attributes) compiled into a filter on those columns:
  indexes serve it as they serve RLS, and its values are read at execution, so a cached plan
  serves every subject whose plan has the same shape. A condition Cerbos expresses with more than
  comparisons, `and`, `or`, `not` and `in` keeps the policy: the plan evaluated per row, or Cerbos
  asked per row. The principal's roles come from the claim `roles_claim` (default
  `role`; `authz.configure(roles_claim => 'app_metadata.roles')`) or, with `subject => 'guc'`,
  from `pg_auth.subject_roles` (comma separated, superuser-only like `pg_auth.subject_id`); its
  attributes are the token's claims. See docs/proposals/cerbos.md.
  An attribute named otherwise than its column (Cerbos's demos write `ownerId`) takes
  `attribute_map => '{"ownerId": "owner_id"}'`, alongside or instead of `attributes`.
  **Scoped policies** (a tenant's overrides, `scope:` in the policy) need the request's scope:
  `authz.configure(scope_claim => 'tenant_id')` sends that claim as Cerbos's scope, on the
  principal and the resource (`pg_auth.subject_scope` for a session subject, superuser-only;
  `policy_version` for a policy version other than `default`). Without one, Cerbos answers from
  the base policy and a scoped override, deny rules included, does not apply; `authz.doctor()`
  says so.
- **Row state** (`not deleted`, `published`) stays plain SQL: add an ordinary restrictive policy
  next to pg_auth's; it joins the scan's filter.
- **Updates and deletes needing their own permission** (GitHub: push to update, delete to delete):
  `on_update => 'can_push', on_delete => 'can_delete'`. `write` still checks inserts; without it, inserts
  are denied unless checked on the parent.
- `force => false` lets the table owner bypass the policies (default: forced).

`authz.unbind(table)` removes pg_auth's policies from a table.

## 4. Grant access by writing rows

An app with no backend changes who may see what by writing rows, through PostgREST like any other
table: a membership, a share, a team. `authz.relate` declares that a table's rows are relationships
in the engine (OpenFGA or SpiceDB), and pg_auth delivers each change.

```sql
-- org:<org_id>#<role>@user:<user_id>, the relation read from the row
select authz.relate('app.memberships', object_type => 'org', object_column => 'org_id',
                    relation_column => 'role', subject_type => 'user', subject_column => 'user_id');
-- document:<doc_id>#shared_with@team:<team>#member, a fixed relation and a team's members
select authz.relate('app.shares', object_type => 'document', object_column => 'doc_id',
                    relation => 'shared_with', subject_type => 'team', subject_column => 'team',
                    subject_relation => 'member');
-- who may write those rows is pg_auth too: only an org's admins manage its members
select authz.protect('app.memberships', type => 'org', id => 'org_id', read => 'can_read', write => 'can_manage');
```

- **One table, several kinds.** Apps keep one `shares` table for users, teams and the public.
  Declare it once per kind with a `filter` on the row, and read any part from a column or fix it:
  `object_type` or `object_type_column`, `object_column` or `object_id`, `subject_type` or
  `subject_type_column`, `subject_column` or `subject_id`, `subject_relation` or
  `subject_relation_column`. `subject_id => '*'` is everyone (`user:*`), `object_id => 'main'` a
  singleton object.

  ```sql
  select authz.relate('app.shares', name => 'team', filter => 'grantee_kind = ''team''',
                      object_type => 'doc', object_column => 'doc_id', relation_column => 'role',
                      subject_type => 'team', subject_column => 'grantee_id', subject_relation => 'member',
                      relations => array['viewer', 'editor']);
  select authz.relate('app.shares', name => 'public', filter => 'grantee_kind = ''public''',
                      object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                      subject_type => 'user', subject_id => '*');
  ```
- **Relations a row may name.** `relations => array[...]` makes a write naming another one fail at once
  (`check_violation`), where the app sees it, instead of the engine refusing it after the commit.
- **Write to relations, read permissions.** The rows write plain relations (`admin`, `member`,
  `shared_with`); tables are protected by permissions computed from them (`viewer: shared_with or
  can_read from org`). SpiceDB only takes writes to relations, and the model reads the same in
  OpenFGA.
- **When it shows.** A trigger records each insert, delete and relationship-changing update in
  `authz.outbox` with the write itself, so a rolled-back write never reaches the engine. The
  sidecar delivers it on commit, and every backend asks again: the change shows within tens of
  milliseconds (`compare/relationships.exs`).
- **Grants that end.** `expires_column => 'expires_at'` names a timestamp column: the
  relationship holds until then (null: for good). The sidecar deletes it from the engine when it
  is due, and every backend asks again, as for any revocation: it goes within a few hundred
  milliseconds of its end. An end moved later keeps it, one moved earlier or into the past ends it
  now, a renewal writes it again; a row whose end has passed is no relationship at all.

  ```sql
  select authz.relate('app.shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                      subject_type => 'user', subject_column => 'user_id', expires_column => 'expires_at');
  insert into app.shares values (1, 'bob', now() + interval '7 days');   -- bob sees doc 1 for a week
  ```

  SpiceDB also gets the end as the relationship's expiration, so the engine ends it itself if the
  sidecar is down (within its revision window, a few seconds). Its schema says so: `use expiration`,
  and `relation viewer: user | user with expiration` (`user with expiration` alone when every row
  has an end: a `not null` column). OpenFGA has no expiry of its own: its grants end when the
  sidecar deletes them, so while the sidecar is down an ended grant stays, like any revocation.
  Ends waiting in the outbox are not changes waiting: `authz.doctor()` counts them apart.
- **Conditions and caveats.** A relationship can be written with an OpenFGA condition or a
  SpiceDB caveat, the row giving its values, and each request the rest: a share in some regions,
  a grant until a deadline, an office's IP range.

  ```sql
  -- the row gives `allowed`; each request gives `region` (the token's claim) and the time asked
  select authz.relate('app.shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                      subject_type => 'user', subject_column => 'user_id',
                      condition => 'in_region', condition_context => '{"allowed": "regions"}');
  select authz.configure(context => '{"current_time": "now", "region": {"claim": "region"},
                                      "client_ip": {"setting": "request.headers", "key": "x-forwarded-for"}}');
  ```

  ```text
  condition in_region(region: string, allowed: list<string>) {     # OpenFGA
    region in allowed
  }
  type doc
    relations
      define viewer: [user, user with in_region]

  caveat in_region(region string, allowed list<string>) {         // SpiceDB
    region in allowed
  }
  definition doc {
    relation viewer: user | user with in_region
  }
  ```

  - `condition_context` maps each parameter the row gives to its column (a null column gives
    none). New values in the row write the relationship again with them.
  - `authz.configure(context => ...)` names what every request gives: `"now"` (the time the
    engine is asked, RFC 3339), `{"claim": "region"}` (the token's claim, `a.b` for a nested one),
    or `{"setting": "request.headers", "key": "x-forwarded-for"}` (a setting, or a key of its
    JSON). A value the request lacks is not sent, and a condition needing it is not met: those
    rows stay hidden.
  - Answers depend on the context, so they are kept per context; one asked with the time is kept
    up to the sidecar's `CACHE_TTL_MS` (1 s), so a grant ends within that of its deadline. The
    permission index is not used while a context is configured: a set is filled for no request
    in particular. Broad subjects are then checked row by row.
  - With rules kept in Postgres, a table naming a condition the rules do not have, a parameter
    the condition does not take, or a relation that does not take `user with <condition>` fails
    at commit.
  - **The protected row's own values.** A condition can also take values from the row being read
    (a report's level against a reader's clearance): protect the table with them as attributes,
    named as the condition's parameters (`attribute_map` when a column is named otherwise).

    ```sql
    -- condition cleared(clearance: int, level: int) { level <= clearance }: the clearance from the
    -- relationship's row, the level from each report
    select authz.protect('app.reports', type => 'report', id => 'id', read => 'viewer',
                         attributes => array['level']);
    ```

    What that costs depends on the engine, since an engine's list cannot take a value per row:

    | engine | how rows are decided | expect |
    |---|---|---|
    | SpiceDB | one LookupResources per subject (with the request's context): rows granted outright and rows not granted at all are decided in Postgres, by an indexable `id = ANY(...)` filter; only rows granted through a caveat that needs the row's values are checked, each with them | the cost of a list, plus one round trip per row whose grant is conditional. A subject with more than `LIST_LIMIT` objects has every row checked |
    | OpenFGA | ListObjects takes no row's values (and fails on a condition missing one), so every row read is checked with its values | one round trip per row read: fine for reads by key and small pages, slow for scans. Prefer SpiceDB for this shape, or a column the relationship can carry instead |
    | Cerbos | attributes are its own model: plans compile into filters on the columns | no round trip per row (docs: Cerbos attributes) |
- **If the engine is down,** changes wait and are delivered when it is back. A change the engine
  refuses (a relation the model does not have) is set aside with its reason, and `authz.doctor()`
  reports it; the changes after it go on.
- **Needs** the sidecar's `PG_AUTH_DATABASE_URL` (as for the permission index), and a unique key
  over the columns, so two rows are never one relationship. Rows already in the table are delivered
  when it is declared; `authz.unrelate(table)` stops delivering.

### The whole setup as one document

`authz.setup(jsonb)` takes who is asking, every relationship table and every protection as one
document, applies it in one transaction, and keeps it with every earlier version
(`authz.setups`); `select jsonb_pretty(authz.setup())` shows the one in force. It compiles into the
calls above, so it can express what they can, and reads as a single thing to review:

```sql
select authz.setup($${
  "subject": {"source": "jwt", "claim": "sub"},
  "relationships": [
    {"table": "app.memberships", "object": {"type": "org", "column": "org_id"},
     "relation": {"column": "role", "allowed": ["admin", "member"]},
     "subject": {"type": "user", "column": "user_id"}},
    {"table": "app.shares", "by": "grantee_kind",
     "object": {"type": "doc", "column": "doc_id"}, "relation": "viewer",
     "subject": {"user":   {"type": "user", "column": "grantee_id"},
                 "team":   {"type": "team", "column": "grantee_id", "relation": "member"},
                 "public": {"type": "user", "id": "*"}}}],
  "protect": [
    {"table": "app.docs", "type": "doc", "column": "id",
     "select": "viewer", "write": "editor", "delete": "owner"},
    {"table": "app.memberships", "type": "org", "column": "org_id",
     "select": "can_read", "write": "can_manage"}]
}$$);
```

- `"column"` names a column of the row; `"id"` is a fixed value (`"*"`: everyone).
- `"by"` declares a table once per value of a column: the subject is a map from those values.
- `"relation"` is a name, or `{"column": ..., "allowed": [...]}`.
- `"expires": {"column": ...}` ends each relationship at the row's timestamp (`expires_column`).
- `"condition": {"name": ..., "context": {"parameter": "column"}}` writes it with an OpenFGA
  condition or SpiceDB caveat (`condition`, `condition_context`).
- A protection takes `"select"`, `"write"`, `"update"`, `"delete"`, `"insert"` (a permission, or
  `{"type", "column", "permission"}` on the parent), `"column"` or `"key"`, `"attributes"` (a list of
  columns, or `{"name": "column"}`), `"hierarchy"` (`{"tree", "direct"}`), `"name"` and `"force"`.
- `"subject"` takes `authz.configure`'s settings: `"source"`, `"claim"`, `"type"`, `"roles"`,
  `"scope"`, `"policy_version"`, `"context"`, `"jwks"`, `"claims_setting"`, `"token_setting"`,
  `"token_header"`.
- What the previous document declared and this one does not is removed. Unknown keys fail, naming
  what the place takes. A relationship table declared exactly as before is not delivered again.

### The engine's rules, kept in Postgres

The model, the schema or the policies can be kept in Postgres too, in the engine's own language,
so a migration carries them with the tables they protect and they are reviewed as written.
`authz.set_rules(engine, name, text)` stores a new version, and the sidecar pushes it to the engine:

```sql
-- OpenFGA: one model, in its DSL
select authz.set_rules('openfga', 'model', $$
model
  schema 1.1
type user
type org
  relations
    define admin: [user]
    define member: [user]
    define can_read: member or admin
type document
  relations
    define org: [org]
    define viewer: can_read from org
$$);

-- SpiceDB: one schema
select authz.set_rules('spicedb', 'schema', $$
definition user {}
definition org {
  relation admin: user
  relation member: user
  permission can_read = member + admin
}
definition document {
  relation org: org
  permission viewer = org->can_read
}
$$);

-- Cerbos: one row per policy, named as you would name its file
select authz.set_rules('cerbos', 'resource/document', $$
apiVersion: api.cerbos.dev/v1
resourcePolicy:
  version: default
  resource: document
  rules:
    - actions: ["view"]
      effect: EFFECT_ALLOW
      roles: ["user"]
      condition:
        match:
          expr: request.resource.attr.owner == request.principal.id
$$);

select engine, name, version, pushed_at, error from authz.rules();   -- the versions in force
```

- **Versions.** Every change is a new version in `authz.rules`, with who stored it and when; the
  same text again is not one. `authz.rules()` shows the latest of each, `body` included.
- **Pushed.** The sidecar pushes the latest version within milliseconds (OpenFGA: a new
  authorization model, which it then reads with; SpiceDB: WriteSchema; Cerbos: its Admin API), and
  every backend asks again, since new rules may change any answer. Each push is recorded in
  `pushed_at`.
- **Checked against the setup.** A protected table naming a type or permission the stored rules do
  not have, or a relationship table writing a relation they compute, or a subject its relation
  does not take, fails when the transaction commits, naming what they have:

  ```text
  ERROR:  the setup names what the rules stored with authz.set_rules do not have:
  public.docs: document has no vewer in the openfga model (it has: editor, org, viewer)
  ```

  The check runs at commit, so a migration can change the rules and the tables using them
  together. A table whose relationships end needs, on SpiceDB, a relation that takes them with an
  expiration (and as they are, when the column may be null). Subjects that need an OpenFGA
  condition or a SpiceDB caveat are not ones pg_auth writes. Cerbos policies are checked for their resource and actions (`approve:*` covers
  `approve:big`). What the stored rules do not show, the engine still checks when they are pushed
  (a relation computed from one that does not exist, a condition that does not compile).
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
- **Cerbos** must store policies in a database with its Admin API on
  (`storage.driver: sqlite3` or `postgres`, `server.adminAPI.enabled: true`), and the sidecar needs
  its credentials (`CERBOS_ADMIN_USER`, `CERBOS_ADMIN_PASSWORD`). A policy directory or git store
  is read-only: keep those rules where they are.
- **Optional.** Rules managed in the engine itself stay there: pg_auth pushes only what
  `authz.set_rules` stored. Once it is used, Postgres is the source of truth for that engine.
- **Order.** Store the rules before `authz.relate` declares tables that write their relations: the
  engine refuses a relationship whose relation its model does not have yet.

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

| setting | default | what it does |
|---|---|---|
| `pg_auth.sidecar_socket` | | the sidecar's Unix socket (superuser) |
| `pg_auth.sidecar_timeout_ms` | 5000 | fail the query when the sidecar takes longer (superuser) |
| `pg_auth.subject_id`, `pg_auth.subject_type` | | the subject for `subject => 'guc'` |
| `pg_auth.subject_roles` | | its roles, comma separated, for an engine that decides on attributes |
| `pg_auth.plan_lookup` | on | shape protected scans at planning; off checks every row through `authz.permits` |
| `pg_auth.batch_check` | on | check rows in batches (AuthzBatch) where ids cannot be listed |
| `pg_auth.permission_index` | on | answer broad subjects from the permission index, and ask the sidecar to fill it |
| `pg_auth.index_list_limit` | 0 | list a broad subject whose fresh index set has at most this many objects (`col = ANY(...)`) instead of checking rows against it; worth it only where broad subjects mostly scan, since every statement builds the array |
| `pg_auth.fill_wait_ms`, `pg_auth.fill_wait_after` | 10000, 2000 | how long, and after how many engine-checked ids, a heavy statement waits for its subject's index set |
| `pg_auth.replan_on_breadth` | on | rebuild this backend's cached plans for a subject much broader or narrower than they were built for |
| `pg_auth.warn_ms` | off | warn when a statement's engine time reaches this |

The sidecar's own settings (list limit, cache TTL, check concurrency) are in its
[README](https://github.com/filipecabaco/pg_auth/blob/main/sidecar/query/README.md).

## 7. What it costs

Per scenario numbers against RLS written the way Supabase recommends are in
[the benchmarks](benchmarks.md); correctness against the engine is checked by
[`compare/parity.exs`](https://github.com/filipecabaco/pg_auth/blob/main/compare/README.md). Writes and very broad subjects cost more than
narrow reads; `authz.explain` and `authz.last_statement_advice()` say when and why.

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
