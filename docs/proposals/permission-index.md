# Proposal: a permission index in Postgres, and advice for heavy queries

Status: phases A and B implemented (2026-10-28): the permission index (`sql/index.sql`,
`sidecar/query/src/index.rs`) and declared hierarchies (`sql/hierarchy.sql`), plus advice for
heavy queries (`src/advice.rs`: `authz.last_statement_advice()`, `pg_auth.warn_ms`,
`authz.explain`'s advice). Phase C is not started. How they work now is in
[design.md](../design.md); this file keeps the reasoning. Background:
`docs/research/broad-subjects-at-scale.md`. A declared hierarchy is now set in the mapping
(`tree: folders(id, parent_id) from viewer_direct`, [Getting started](../usage.md#the-mapping)); the
`authz.protect` calls below are what it compiles into.

## Problem

A subject with more objects than the engine lists (`LIST_LIMIT`, 1000) is enforced by AuthzBatch:
the ids of the rows a scan reaches are checked in batches. That is right for queries that reach
few rows, but a query that reads most of a large table costs one engine resolution per distinct
id (~1.7 ms each on OpenFGA, ~2.7 ms through a folder hierarchy): 50k folders take over a minute.
Every engine vendor answers this case the same way, by moving the decision into the database
(OpenFGA's local index from ReadChanges, AuthZed Materialize, Zanzibar Leopard, Oso local
authorization). Postgres may now hold derived permission data, so pg_auth can do the same.

## Who this is for

Supabase users: pg_auth should let them drop authorization from their application layer
entirely, because every Supabase surface already reaches data through Postgres and its row
level security (PostgREST, GraphQL, Storage's `storage.objects`, Realtime's per-subscriber
checks of changes). Enforcing in the database, close to the data, is the BaaS promise. That
shapes the design:

- Decided 2026-10-28: Supabase runs the engine and the sidecar next to each project's Postgres
  (as it runs PostgREST), so the Unix-socket design stays. Round trips stay local, but they are
  still one per batch of rows: the decision data for broad subjects has to live in Postgres.
- Realtime evaluates the policy for each subscriber on each change: hot, small, repeated
  checks, where an in-database index answers without any round trip.
- The subject already comes from the JWT (`authz.configure(subject => 'jwt' | 'jwt_signed')`),
  which is how PostgREST passes Supabase Auth's user.
- Supabase users write policies in SQL in the dashboard, so the declarative SQL API matters as
  much as speed.

This makes the permission index (phases A and B) the core of the Supabase path rather than an
optimisation: with it, most statements need no engine call at all, and the engine (or its
change feed) only keeps the index fresh.

## Goals

- Broad subjects reading many rows cost an indexed join, not engine calls.
- The engine stays the source of truth; the index is derived, refreshable and has a known freshness.
- Fail closed: a missing, stale or broken index falls back to the engine path, never to more rows.
- Cached plans stay correct for every subject (no subject-specific constants in plans).
- pg_auth can say why a query is heavy and what to change.

## Design, in three phases

### Phase A: per-subject permission sets, filled on demand

Model-agnostic; works for any OpenFGA or SpiceDB model.

    authz.permission_sets(subject text, object_type text, permission text,
                          revision text, filled_at timestamptz, ids text[] or a child table)
    authz.permitted(set_id bigint, object_id text, primary key (set_id, object_id))

- When a statement meets a subject the engine cannot list, pg_auth asks the sidecar for the
  full set with a streaming list (OpenFGA StreamedListObjects, which has no result cap; SpiceDB
  LookupResources with cursors) instead of failing over to batch checks, stores it, and plans
  `col = ANY(...)` against it (or a hashed semi-join for large sets). One list call per subject
  per refresh replaces one check per row per query.
- Filled lazily, only for subjects that query, so storage is bounded by active broad subjects
  times their set sizes rather than all subjects times all objects (a public object would
  otherwise appear once per user).
- Freshness: each set records the engine revision it was read at. A background worker (or the
  sidecar) tails the engine's change feed (OpenFGA `ReadChanges`, SpiceDB `Watch`) and marks sets
  of the affected types stale; a TTL bounds staleness when the feed is unavailable. A stale set
  is refilled; until then, the statement uses the engine path (AuthzBatch), so revocations are
  never missed.
- Cost, measured 2026-10-28 (OpenFGA v1.22.0 on Postgres, caches on; the hierarchical bench's
  tree of 50k folders): the broad subject's 40,158 folders take 3.2 s to list, streamed or not
  (45 ms when OpenFGA's list cache is warm); the selective subject's 364 take ~100 ms. Checking
  those folders one by one would cost ~40k x 2.7 ms ≈ 110 s per query. So one fill per subject
  per refresh replaces minutes of checks per query. 3.2 s is past OpenFGA's default 3 s
  `listObjectsDeadline`: filling needs a longer deadline (the sidecar already treats an answer
  that took the deadline as incomplete). Writes in the engine cost invalidations, not
  recomputation.

### Phase B: Leopard-style decomposition for declared hierarchies

For relations that inherit through data Postgres already has (a folder's `parent_id`), store
what Zanzibar's Leopard stores instead of every (subject, object) pair:

    authz.grants(subject, set)                 -- direct grants: few per subject
    authz.closure(ancestor, descendant)        -- transitive closure of the hierarchy

and enforce `col IN (SELECT descendant FROM authz.closure WHERE ancestor = ANY(subject's grants))`.
The bench's broad subject has 2 direct grants and ~33k inherited folders: two ids and an index
join. The engine is asked only for the grants; the closure is maintained from the app's own
table (triggers or the change feed). This needs the model to say which relation is the
hierarchy edge and which table column holds it (Oso's fact mapping), which is also the natural
place for a nicer syntax than RLS, e.g.

    select authz.protect('app.files', 'folder', 'folder_id', 'viewer',
                         hierarchy => 'app.folders(id, parent_id)');

Write amplification is the known cost (one edge can add ancestors x descendants closure rows);
Zanzibar limits it to selected relations, and so would pg_auth (only declared hierarchies).

**Implemented (2026-10-28), `sql/hierarchy.sql`.** `authz.protect(..., hierarchy =>
'app.folders(id, parent_id)', direct => 'viewer_direct')` builds `authz.closure_<oid>` and
row and statement triggers on the app's table that keep it current, refuse a move that would
make a cycle, empty it on TRUNCATE, and bump a version per statement. The model must read
`viewer: viewer_direct or viewer from parent`. For a declared `type#permission` the extension
lists the subject's direct grants from the engine, expands them in Postgres
(`authz.hierarchy_expand`), and keeps the expansion per backend across transactions, keyed by
the tree's version and the exact grants. Up to 1000 objects: listed ids. More: the expansion is
the subject's index set for AuthzBatch, with no fill and independent of
`pg_auth.permission_index`. A subject with too many direct grants to list falls back to the
engine path. Measured on the hierarchical bench (50k folders): the broad subject's first
statement in a new backend takes 22 ms, against 6.4 s waiting for its permission index fill;
warm statements are the same as with a fresh index. `authz.drop_hierarchy` removes a declaration.

### Phase C: compile attribute rules

Rules on the row's own columns (owner, visibility, status, tenant) compile to plain SQL
conditions, as OPA's Compile API and Cerbos query plans do, with no engine call at all.
Restricted to comparisons, `in`, `exists` over named relations, and/or; anything else fails
closed (falls back to the engine).

## Choosing a path per statement

Per (subject, type, permission), as vendors do by result size:

| subject's set | path |
|---|---|
| ≤ `LIST_LIMIT` | listed ids (today) |
| larger, index fresh | permission index (A or B) |
| larger, index missing or stale | AuthzBatch while it is filled |
| writes, row locks | per-row check (today) |

## Advice for heavy queries

pg_auth knows per statement: the subject's breadth (listed count or "too many"), the path taken,
ids checked, sidecar and engine time (`authz.stats()`), and the binding's model shape. Implemented:
`authz.last_statement_advice()`, `pg_auth.warn_ms` and `authz.explain`'s advice column, whose advice
explains engine checks through the permission index (off, missing in the sidecar, being filled,
stale). The rules below are not implemented yet.

- `pg_auth.warn_ms` (default off): statements whose engine time exceeds it raise a WARNING with
  the cause and the fix; `authz.explain(query)` gains an `advice` column.
- Rules, in order:
  1. the permission is inherited through a hierarchy and the subject's set is large: declare the
     hierarchy (phase B), or enable the permission index;
  2. the subject's set is just above `LIST_LIMIT` (count known from the streaming list): raise
     `LIST_LIMIT` (and OpenFGA's `listObjectsMaxResults`) to N;
  3. the protected column has many distinct ids and the model reaches it through a coarser
     relation with a column (org, project): protect by that column;
  4. the scan reads most of the table: add a selective condition or an index for it.

## Decisions (2026-10-28) and remaining questions

1. Decided: one row per object, `authz.permitted(set_id, object_id)` keyed by both, so change
   feed updates touch only changed rows.
2. Decided: the sidecar tails the change feed and writes into Postgres through a small SQL API.
3. Decided: sets the feed marks stale are not used; statements take the engine path until the
   set is refilled. No read-after-write tokens for now.
4. Decided: `hierarchy => 'table(id, parent)'` as an argument of `authz.protect`.
5. Decided: Supabase runs the engine and sidecar next to each project's Postgres (Unix socket).
6. Distribution: pg_auth is a pgrx (Rust) extension with `shared_preload_libraries`; on managed
   Supabase that means Supabase ships it (users cannot install compiled extensions themselves).
   Self-hosted Supabase can load it today. (Assumption, to confirm with Supabase.)
