# pg_auth: enforce an authorization engine's decisions at the query planner

Status: draft · 2026-10-28

## 1. Problem

Postgres row-level security (RLS) runs authorization at the right layer: policies become quals the
planner optimizes with the rest of the query, and they are enforced for every client. But the
policies are hand-written SQL, one table at a time. Teams that model authorization as
relationships (Zanzibar, OpenFGA, SpiceDB) or as policies (Cedar, OPA) end up maintaining two
sources of truth, or skip RLS and authorize in the application, which defeats the point.

**Goal:** protect tables with the decisions of an external authorization engine, enforced inside
Postgres for every client, at a cost close to a hand-tuned policy. For Supabase users this means
no authorization in the application layer at all: PostgREST, GraphQL, Storage and Realtime all
reach data through Postgres, so pg_auth's policies cover every surface.

### Non-goals
- Being an authorization engine. Postgres evaluates no model and stores no relationships; the
  engine is the only source of truth. It may hold *derived* answers (the permission index, §3),
  which the engine's change feed keeps fresh and which are never used once stale.
- Replacing the RLS mechanism. pg_auth installs ordinary policies and shapes them at planning time.
- Column-level or DDL authorization.

### History

The first prototype compiled OpenFGA models into SQL over a mirror of the engine's tuples,
precomputing every permission in Postgres. Measured against OpenFGA itself (`compare/`), it
answered checks 20x slower, could not list objects at request time, and duplicated the engine's
semantics (set operations, wildcards, conditions) with all the drift that implies. It was removed.

## 2. Architecture

```
            ┌───────────────── Postgres backend ─────────────────┐
 query ──▶  │ parse ─▶ rewrite (RLS adds the policy) ─▶ plan ─▶ execute
            │                                      │           │
            │                       planner_hook: list ids     │ authz.allowed_* (InitPlan)
            │                                      │           │ authz.permits   (per row)
            │                                      ▼           ▼
            │                          src/sidecar.rs: one socket per backend,
            │                          answers cached for the transaction, lists
            │                          kept across them while the sidecar's are valid
            └──────────────────────────────────┬─────────────────┘
                                               │ JSON lines, Unix socket
                                     pg-auth-sidecar (sidecar/query)
                                               │ HTTP
                                     OpenFGA or SpiceDB
```

- **Binding.** A note in the engine's rules (`# pg_auth: protects docs (org_id): read view, write
  edit`, in the `org` type), or the lower-level `authz_internal.protect('public.docs', 'org', 'org_id',
  'view', 'edit')` it compiles into (table, object type, id column, read permission, optional write
  permission), installs the policy `authz.permits('<type>', '<permission>', column::text)` on the table and on
  every partition, now and later.
- **Schemas and roles.** `authz` holds what every role reaches: the functions policies and the
  planner hook call (run as the querying role), and the user-facing ones, whose admin functions
  are granted to `authz_admin`. `authz_sidecar` holds the sidecar's functions (role
  `authz_sidecar`), `authz_internal` the tables and the lower-level setup (role `authz_admin`).
  Access is decided per schema (sql/grants.sql), so a new internal function is closed by default.
- **Sidecar.** A separate process next to Postgres that speaks one small protocol (`list`,
  `check`, `fill`, `ping`) and translates it to an engine's API: OpenFGA's or SpiceDB's, chosen by
  its configuration (§4). Network calls and engine credentials stay outside the database. The
  extension does not know which engine answers. It reaches the sidecar on a Unix socket, or over
  HTTPS by URL (`pg_auth.sidecar_url`, `src/remote.rs`): there each request is signed with a secret
  both ends hold and each answer with the same secret over it and its request, and anything else
  fails the statement (docs/proposals/external-providers.md).
- **Subject.** Who is asking comes from a superuser-only GUC (granted per API role), a JWT claim,
  or a bearer token verified in Postgres against a JWKS (`authz.configure`). The planner and the
  scans run as the querying role, so they resolve it through the security definer functions
  (the role never reads `authz_internal.settings`, which may hold secrets). The answer is kept, across
  transactions too, while the request settings it read are unchanged, the configuration has not
  changed and a verified token has not expired, so a returning subject costs no query at all
  (§5, configuration).
- **Relationship tables** (sql/relationships.sql, sidecar `outbox.rs`) are the other direction:
  rows the application writes that are relationships in the engine. A row trigger records each
  change in `authz_internal.outbox` in the writing transaction; the sidecar claims the oldest changes (an
  advisory lock: one sidecar delivers, in order), writes them to the engine idempotently, and
  removes them by id in one transaction, which also calls `authz_sidecar.answers_changed()`: a relcache
  invalidation of `authz_internal.outbox` that makes every backend drop the answers it kept across
  transactions. Reads then skip the engine's caches (OpenFGA's `HIGHER_CONSISTENCY` for 10 s,
  SpiceDB at least as fresh as the write's ZedToken), so a grant or a revocation shows within tens
  of milliseconds of its commit. A change the engine refuses for good is set aside with its reason,
  never holding up the next ones.

## 3. Planning: shape the scan with the engine's answer

The planner hook finds pg_auth policies among each relation's security quals (including
subqueries and CTEs) and asks the sidecar once per table: which ids of this type may the current
subject access with this permission?

| Answer | Plan | Cost |
|---|---|---|
| no subject, or no ids | the filter below, which returns no ids | no rows read |
| one id at most, a single-table read with ORDER BY that does not otherwise compare the column | `col = (SELECT authz.allowed_one_<type>(...))` | one InitPlan; keeps the order of an index on `(col, x)`, so `ORDER BY x LIMIT n` stops after n rows (`col = ANY(...)` cannot: Postgres sorts the subject's rows) |
| a list (below the engine's limit, 1000 for OpenFGA) | `col = ANY((SELECT authz.allowed_<type>(...)))` | one InitPlan; index scans, partition pruning |
| too many for the engine to list, with a fresh permission index set of at most `pg_auth.index_list_limit` objects (off by default) | listed from that set, as above | no row checks, but every statement builds the array |
| too many to list, plain reads | an `AuthzBatch` custom scan over the table scan (below) | rows answered from the permission index when the subject's set is fresh; otherwise one round trip per batch of rows the scan reaches |
| too many to list, an UPDATE or DELETE not pinned to one row by a unique key, with a fresh permission index set of at most 10,000 objects | listed from that set (`col = ANY(...)`) | the rows found through an index on the column, as RLS does, instead of `authz.permits` on every row of the table |
| too many to list, other writes and row locks | the policy stays: `authz.permits(...)` per row | from the permission index when fresh; otherwise one round trip per distinct id, cached per transaction |
| sidecar or engine error | the policy stays and raises at execution | fails closed |

The `= ANY` is hashed: Postgres hashes it only over a constant array, but the InitPlan takes no
parameters, so its ids are the same for every row of the query and the hash table is built once
(a broad seller's order page over a list of thousands of ids: 127 ms unhashed, 2.7 ms hashed). Ids are
kept only when written as Postgres writes the column (`col::text`, which the per-row check
compares): `"007"` is not 7.

The filter replaces the policy inside the security quals, so RLS's ordering still keeps
non-leakproof user functions away from rows the subject may not see. Postgres cannot see into
the InitPlan, so it would estimate `col = ANY(...)` as about ten ids, and pg_auth's per-row
check as a fixed fraction, whatever the subject's count; pg_auth raises the relation's row
estimate to the share of rows it knows the subject has while planning (its listed ids, each with
its own share where the column's statistics list it as a common value, or a fresh permission
index set's size over the column's distinct values), so an admin who may see 90% of the rows gets
an ordered index walk that stops after the page, and a subject of two orgs a fetch of their rows.

**AuthzBatch.** A `set_rel_pathlist_hook` wraps each scan of a relation that still carries the
per-row policy (SELECT without row locks) in a custom scan. The inner scan reads the columns
AuthzBatch uses (index-only scans stay index-only) and evaluates only leakproof conditions on that
table, minus those its index already enforces; AuthzBatch reads its rows in batches
(16, doubling to 1000), checks their ids with one sidecar request, and evaluates every other
condition and the projection only on the rows that passed, which keeps the same ordering
guarantee. It keeps the inner scan's order, so sorted index scans and `LIMIT` stop early: a
`LIMIT 20` checks a batch or two, not the table. For a subject whose ids can be listed it answers
from the listed set, so a cached AuthzBatch plan is right for every subject.
`pg_auth.batch_check = off` restores the per-row policy. Writes and `FOR UPDATE`/`SHARE` keep it,
since AuthzBatch does not do EvalPlanQual rechecks. Reads that use system columns (`ctid`) keep it
too, since batched rows do not carry them.

**Permission index** (`sql/index.sql`, `docs/proposals/permission-index.md`). For a subject with
too many objects to list, pg_auth first looks for the subject's fresh set in `authz_internal.permitted`
(the index functions resolve the current subject themselves, so no one can read or probe another
subject's set). It probes each batch's ids by primary key, and once a transaction has probed an
eighth of the set it loads the whole set into memory: a primary key lookup costs one probe, a
full scan one load. From then on AuthzBatch stops buffering and checks each row against the
loaded set as it reads it, copying only the allowed rows; integer id columns are matched as
integers, without rendering each value as text (only set entries written the way Postgres writes
the integer count, so `"007"` never matches 7). It does so in a function it hands Postgres
for the node's next rows: it reads the id from the child's row, probes the set, and runs
AuthzBatch's own condition and projection on that row, with no copy and no pgrx error guard per
call into Postgres (the same as the listed pass-through below; the condition and projection are
compiled for any slot type, since the child's slot is not the scan slot). The backend keeps loaded sets across transactions
(up to 1M ids), named by set and fill (`set_id`, `filled_at`: a refill keeps the id): each
transaction still finds the subject's fresh set first, so only current contents are reused, and
a broad subject's 25k-id set is loaded once per connection instead of once per request. No
engine call either way: a fresh set exists only for a subject the engine
could not list, so pg_auth does not ask the engine to list again (that call alone took ~25 ms for
a broad subject, OpenFGA finding 1000 objects before it stops). Without a fresh set pg_auth asks the sidecar to
fill it and checks with the engine meanwhile. A statement that has sent the engine
`pg_auth.fill_wait_after` ids (2000, ~3 s of OpenFGA checks) of one subject is heavy: it waits,
once per transaction and up to `pg_auth.fill_wait_ms` (10 s; the wait answers cancels and
statement timeouts), for that fill, then reads the set with a new snapshot (READ COMMITTED sees
it; under REPEATABLE READ the engine keeps answering) and answers the rest of its rows from it. A
broad subject's first count over 100k files took 91.9 s of engine checks without it, 4.2 s with
it (2008 ids checked, then 465 ms waiting for a fill already under way). Row-by-row checks (a
write's policies) wait the same way, after a tenth as many requests, each being one round trip:
a broad subject's first update of 250k rows took 2.8 s (5000 single-id checks), now 1.0 s (200,
then 38 ms waiting), about its steady state. The sidecar sends a check request's BatchCheck
calls (50 ids each) `CHECK_CONCURRENCY` (8) at a time, which halved the engine-checked scans of
nested-rls-chain without the index (comments: 2.7 -> 1.3 s). A set that goes stale
mid-transaction hands the remaining batches back to the engine. The sidecar fills a set
by streaming the engine's complete list (a broad subject's 40k folders: 3.2 s, against ~110 s of
per-row checks for every query) and follows the engine's change feed, marking every set stale on
any change; a set invalidated while it fills never becomes fresh. The feed keeps the position it
has read to in Postgres, and a starting feed resumes from it, so a restart, or another sidecar
starting, keeps the sets when nothing changed and catches what changed while none ran. So the index answers broad
subjects at in-memory speed, and a revocation applies once the feed is read (~0.2 s) and the
engine's own caches expire (OpenFGA: 10 s TTL when on). `pg_auth.permission_index = off` turns it
off.

**Plan caching.** The ids are never constants in the plan. The InitPlan calls `allowed_<type>` at
execution for whoever runs it, so a generic plan cached for one subject is correct for the next
(prepared statements across requests, one role, a subject per transaction: the PostgREST
pattern). Since a subject of another breadth class gets its own plan (below), a listed subject's scan
is left as Postgres planned it (a two-table join planned and run in 80 us, where wrapping each
scan in AuthzBatch took 99 us; best-practice RLS 72 us). With `pg_auth.replan_on_breadth = off`,
and for one-id plans (which subjects with a few more ids share), the listed-ids scan sits under
AuthzBatch next to the same scan without the filter, and AuthzBatch picks one at execution: a listed subject reads the filtered
scan, whose rows (already authorized by the filter, and filtered with the planner's own conditions
in RLS's security order) pass straight through with no per-row work (after the first row the node
hands Postgres a function that calls the child directly, skipping ExecCustomScan and pgrx's
per-call error guard); any other subject reads the
unfiltered one with batch checks, so the InitPlan never runs for it. Such paths are not
projection capable, so computed output comes from a Result node above. A plan in the one-id
form is answered from it only for a subject with at most one id; one with more reads an
unfiltered index walk in the plan's order on an index not involving the column (else the same
scan without the filter, sorted: the one-id scan's order may come from its equality), checked
against its set. The one-id form is limited to single-table pages because elsewhere (joins,
aggregates) a reused one-id plan leaves subjects with more ids an unfiltered scan, where
`col = ANY(...)` serves every listed subject; and to queries that do not compare the column
themselves, since `col = $one` would join its equivalence class and spread to other relations,
outside AuthzBatch.

**Adaptive pages.** A cached plan serves subjects of every breadth (a pooled connection's
prepared statements), so for a single-table read with ORDER BY pg_auth also offers an AuthzBatch
path already in that order: child 0 fetches the subject's rows through its listed-ids filter and
sorts them, child 1 walks an index in that order (not on the protected column) unfiltered,
checking rows in batches until LIMIT is met. The walk keeps the query's own conditions as index
conditions (a name range on (bucket_id, name) reads that range), built by Postgres's index path
builder with the listed filter left out. At execution it compares the rows each would read (the
subject's share of the rows the query's conditions leave, against LIMIT over that share) and runs
the cheaper one. The share is its ids' shares, from the column's most common values recorded at
planning where an id is one (a tenant with 8% of the rows walks, a tenant with 0.05% fetches),
an even part of the rest otherwise: a plan cached for a one-org subject no longer sorts the whole
table for an admin, nor walks the whole index for a one-org subject. Without AuthzBatch (a read of system columns), such a reuse fails
rather than show a subset. For writes, a subject whose set is too large to list makes `allowed_<type>`
return its fresh permission index set, or batch-check the column's distinct values without one.

**A query pinned to one id.** When the query's own conditions fix the protected column to one
value (`where tenant_id = 1`, for an exact type: integers, uuid, text in a deterministic
collation), AuthzBatch checks that value once at execution instead of each row: allowed, the rows
pass unchecked; denied, none is read. A broad subject's empty tenant page read 5,137 buffers
checking rows one by one; RLS, with both conditions on one index, read 8.

**Estimates follow skewed ids.** The row estimate pg_auth raises from the subject's ids uses each
id's share from the column's most common values where it has one (a tenant with half the table),
not the average share, so the planner does not plan a bitmap AND and a sort for 4 rows that are
200,000.

**Cached plans follow the subject's breadth.** Other plan shapes (joins, aggregates) cannot choose
at execution, and Postgres does not know a cached plan depends on its subject: a hash join over a
one-account bitmap scan, reused by an admin who sees every account, read 20 ms where a plan built
for the admin reads 0.5 ms. The planner records, per type#permission, the planning subject's
breadth class (up to 4 ids, up to 64, more, too many to list). At the end of each top-level
statement (PostgREST sets the request's claims with one), and at a SET made inside a statement,
the current subject's classes are compared; when one differs, `ResetPlanCache()` marks this
backend's cached plans invalid, so each is planned again for the new subject on its next use.
Subjects of one class share plans. After a top-level SET (no snapshot to read the subject with)
the comparison waits for the end of the next statement. `pg_auth.replan_on_breadth = off` reuses
plans as built (parity's cached-plan modes do, to check those answers).

**Attribute rules** (docs/proposals/cerbos.md). A table protected by its rows' attributes
(`authz_internal.protect(..., attributes => array[...])`, for an engine that decides on them: Cerbos)
has the policy `authz.permits_resource(kind, action, id::text, jsonb_build_object(...))`. The
engine's plan for the subject (always, never, or a condition on the attributes) is compiled into
a filter on the row's columns, which indexes and partition pruning serve; each of the plan's
values is read at execution by an InitPlan, and a guard refuses a subject whose plan has another
shape, whose cached plans are rebuilt first (compared like breadth classes, and discarded at once
by a top-level SET of a setting the subject comes from). A plan outside the compiled language
(arithmetic, say) keeps the policy, which evaluates the plan per row in Postgres or asks the
engine with the row's attributes.

`authz.explain(query)` reports the decision and its reason per table. `authz.stats()` counts this
backend's round trips to the sidecar, the ids it sent to be checked, the time spent waiting on the
sidecar and, as the sidecar reports it, on the engine, and the ids answered from the permission
index; `bench/docker` diffs it per statement.

**Advice** (`src/advice.rs`). Executor hooks frame each top-level statement; pg_auth records,
per subject and permission, the ids it sent to the engine, the engine time, the ids answered
from the index, and whether a fill was requested. `authz.last_statement_advice()` returns that
for the last statement that used pg_auth, with one line of advice wherever the engine had to
check rows, naming the cause and the fix: the subject's index set is being filled (later reads
answer from Postgres), it is stale after a change, the sidecar keeps no index
(`PG_AUTH_DATABASE_URL`), or `pg_auth.permission_index` is off. It works behind PostgREST, where
warnings never reach the client. `pg_auth.warn_ms` (off by default) also raises the advice as a
WARNING when a statement's engine time reaches it, and `authz.explain` has an `advice` column
saying, before running, whether a table will be answered from the index or from the engine.

**Declared hierarchies** (`sql/hierarchy.sql`). When a permission is inherited down a tree the
app already stores (`viewer: viewer_direct or viewer from parent`, with the parent edge being a
folder's `parent_id`), `authz_internal.protect(..., hierarchy => 'app.folders(id, parent_id)', direct
=> 'viewer_direct')` keeps a transitive closure of that table, maintained by triggers on it, as
Zanzibar's Leopard does. The engine is then asked only for the subject's direct grants, usually a
handful even for a subject who sees most of the tree, and Postgres expands them. A small
expansion is listed ids. A large one becomes the subject's index set for AuthzBatch, with no fill
to wait for. Expansions are kept per backend across transactions until the tree changes (a
version bumped by every statement that changes it) or the grants do.

## 4. Protocol

One JSON object per line, one response per request (`sidecar/query/src/protocol.rs`):

    {"op":"list","subject":"user:anne","permission":"viewer","type":"document"}
      -> {"ids":["1","2"],"engine_us":840,"valid_ms":500} | {"too_many":true,"engine_us":900} | {"error":"..."}
    {"op":"check","subject":"user:anne","permission":"viewer","type":"document","ids":["1","9"]}
      -> {"allowed":[true,false],"engine_us":1200}
    {"op":"fill","subject":"user:anne","permission":"viewer","type":"document"}
      -> {"queued":true}                    (the permission index; false when the sidecar keeps none)
    {"op":"fill",...,"wait_ms":10000} -> {"queued":true,"fresh":true}   (once the fill ends or wait_ms passes)
    {"op":"ping"} -> {"engine":"openfga"}

`list` and `check` may carry `"context":{...}` and `"now":"current_time"`: values for OpenFGA
conditions and SpiceDB caveats, resolved by the extension from `authz.configure(context => ...)`
(a token's claims, settings), and a parameter the sidecar gives the time it asks the engine.
Lists are cached per context.

`engine_us` is the time the request spent waiting on the engine (0 when the sidecar answered from
its cache); it is informational. `valid_ms` is how long the extension may keep a list: until the
sidecar's cached answer reaches half its TTL, when the next request refreshes it in the
background.

For OpenFGA: `list` is ListObjects, which truncates silently both at its result limit and at its
deadline (answering with the objects found so far), so a full page or an answer that took the
deadline means `too_many`; `check` is BatchCheck in chunks of 50; a fill is StreamedListObjects;
the change feed is ReadChanges, polled, resumed from the continuation token kept in Postgres
(a token OpenFGA refuses marks every set stale).

For SpiceDB (its HTTP gateway): `list` is LookupResources, paged by its cursor past the server's
1000-per-call limit, every page at the first one's revision, `too_many` at the sidecar's
`LIST_LIMIT`; `check` is CheckBulkPermissions in calls of 1000 ids, which SpiceDB dispatches as
one check per subject, type and permission; a fill is LookupResources at the latest revision
(`fully_consistent`), so it reflects every change the feed reported before it began; the change
feed is Watch, streamed and resumed from the last revision seen, or on start from the cursor kept
in Postgres (a connection that cannot resume marks every set stale). A caveated relationship evaluated without its context is conditional, and
conditional is denied. Subjects may be usersets (`group:eng#member`).

## 5. Security and correctness

- **Fail closed.** Unreachable sidecar, engine error or malformed answer: the statement errors.
  No subject: no rows. Missing check results: denied.
- **Who may steer decisions.** `pg_auth.sidecar_socket` and `pg_auth.sidecar_timeout_ms` are
  superuser-only; the socket is created 0660 for the Postgres OS group. `authz.check` answers for
  any subject and is revoked from `public`; `authz.can` answers for the current subject only.
- **Consistency.** Within a transaction a subject gets one answer per (type, permission). Across
  transactions answers are as fresh as the engine, minus the sidecar's cache TTL (1 s by default).
  The engine's own freshness: OpenFGA's caches (10 s TTL when on); SpiceDB's default
  `minimize_latency` reads a revision up to its quantization window old (5 s; `SPICEDB_CONSISTENCY
  = fully_consistent` reads the latest).
  Writes to the engine and to the table do not commit atomically. The sidecar refreshes a list
  answered from its cache past half its TTL in the background, so a subject asking steadily
  never waits on the engine and no answer is older than the TTL.
- **Configuration is kept per backend.** `authz_internal.settings`, the resolved subject (while the
  request settings it came from are unchanged) and the declared hierarchies are kept across
  transactions; statement triggers on `authz_internal.settings` and `authz_internal.hierarchies` send a relcache
  invalidation that makes every backend reload them once the change commits (with the latest
  snapshot, whatever the isolation level). A signed token without `exp` is rejected.
- **Declared hierarchies trust Postgres's tree.** For a declared permission the app's table, not
  the engine's parent tuples, decides inheritance; a tree change commits with the write that
  made it. pg_auth cannot verify that the model has the declared shape. If the engine's
  tuples and the table disagree, the table wins. The bench and parity compare both against the
  engine.
- **Unsafe Rust** is confined to `src/pg/` (handles on Postgres trees, hook entry points, the
  AuthzBatch executor) and `src/ffi.rs` (SPI); the planner and executor logic in `src/rewrite/`
  is safe code over them, and the sidecar forbids `unsafe` (development.md).

## 6. Open questions

1. Row locks, inserts, writes pinned to one row, and writes by subjects whose index set is stale
   or over 10,000 objects still check rows one by one (answered from the permission index once it
   is fresh); AuthzBatch does not do EvalPlanQual rechecks.
2. Where pg_auth still loses to best-practice RLS (`bench/docker` results, commit `eb69ff7`;
   0.80x overall): a tenant holding half the table paging one kind of row
   (`tenant-skew-bad-estimates`, 1.7x), direct reads of one partition (1.6x), writes that read
   their row back (`update-select-coupling`, 1.6x), joins of two protected tables (1.2x), and
   per-statement planning on sub-millisecond statements (RLS's own best case, 1.4x). These
   sub-millisecond scenarios move by about a third between runs: `jwt-claims-parsed-per-row` read
   1.3x in the previous run and 0.90x in this one, and an A/B of `update-select-coupling` against
   the build before request context (`59e6f54`, alternating, fresh containers) read 1.49x/1.40x
   old and 1.86x/1.47x new, the best-practice policy's own times varying as much.
3. SpiceDB's gRPC API instead of its HTTP gateway (one JSON translation less per call). Reads
   are already at least as fresh as the sidecar's last write (its ZedToken); a write made outside
   pg_auth, straight to SpiceDB, is not tracked.
4. Phase C of the permission index proposal: attribute rules compiled to SQL conditions.
5. Attribute rules from Cerbos (built, docs/proposals/cerbos.md): planning per protected table
   stays above RLS's on sub-millisecond statements.
6. Planning cost common to every engine. Done: pg_auth's own queries are prepared once per
   backend (SPI_prepare, SPI_keepplan), the subject is resolved in Rust once the configuration is
   in memory, and `authz.warm()` does a backend's one-time work ahead of its first request (a new
   connection's first planning 0.4 ms -> 0.15 ms; 0.12 ms when it also prefetches the subject).
   Steady-state planning did not move (bench medians equal to `c0fc79f`): the per-transaction
   queries were not the cost. Measured on a kept `rls-best-case-tenant-column` stack, a compiled
   filter plans in 0.024 ms back to back (hand-written SQL 0.014 ms); what remains is time, not
   work: after an idle backend crosses the kept answer's `valid_ms` (half the sidecar's
   `CACHE_TTL_MS`, 1 s), planning waited on an engine round trip (+2.2 ms with Cerbos; the
   sidecar refreshed lists ahead of expiry, never plans). Now the sidecar refreshes every list and
   plan used within `REFRESH_ACTIVE_MS` (30 s) before it expires, asked for or not, keeping the
   1 s freshness bound: after 1.5 s idle, planning 2.65 -> 0.47 ms, no engine time; what remains
   is the socket round trip after a pause and the host's cold caches.
