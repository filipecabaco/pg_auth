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
- **Row state** (`not deleted`, `published`) stays plain SQL: add an ordinary restrictive policy
  next to pg_auth's; it joins the scan's filter.
- `force => false` lets the table owner bypass the policies (default: forced).

`authz.unbind(table)` removes pg_auth's policies from a table.

## 4. Inspect and debug

```sql
select * from authz.doctor();         -- the setup checked, with the statement that fixes each problem
select * from authz.why('app.documents', '42', as_subject => 'user:anne', as_role => 'authenticated');
--  whether and why that row is visible: subject, row level security, privilege, each policy, verdict
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

## 5. Settings

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

## 6. What it costs

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
