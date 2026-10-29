# Broad subjects at scale: how engines filter large lists, and what pg_auth should do

Research date 2026-10-28. Four parallel research passes (OpenFGA; SpiceDB and Zanzibar; OPA and
Cedar; Cerbos, Oso and Permify), read against our own measurements. It builds on
`scaling-authz-services.md` (Zanzibar Leopard, SpiceDB internals), `scaling-db-native.md`
(database-native prior art) and `policy-engines.md`. "(unverified)" marks claims not confirmed
from a primary source; the research used WebFetch summaries, so wording is not verbatim.

## The problem, measured

pg_auth asks the engine either for a subject's allowed ids (`col = ANY(ids)`) or, for subjects
with too many objects to list, about the ids of the rows a scan reaches (AuthzBatch). On
OpenFGA v1.15.1 with its memory datastore (`bench/docker/README.md`):

| | cost |
|---|---|
| listed ids | one ListObjects per statement, 0.3–0.8 ms |
| batch check | ~1.7 ms per id (one client); 12–24 ms per id with 8 concurrent clients |
| single check | ~4.3 ms per id (one client); 7–8.5 ms with 8 clients |
| folder inheritance (`viewer from parent`, depth ~10) | ~2.7 ms per id |
| broad subject, `count(*)` over 100k files in 50k folders | ~50k checks, over the 60 s timeout |

Batching removes round trips, not checks. A broad subject reading a large share of a big table
costs about one engine resolution per distinct id, whatever the transport.

## What every vendor converges on

| result size | recommended path | sources |
|---|---|---|
| small (≲ 1k–10k ids) | list ids, filter in the database (`id = ANY(...)`) | OpenFGA "ListObjects then search" (~1k); AuthZed LookupResources (< ~10k); Oso central `list` (~10k) |
| large, but the query narrows candidates (search, pages) | search first, then bulk-check the page | OpenFGA "search then check"; AuthZed CheckBulk over a DB page |
| large, and the query reads many rows | push the decision into the database: a predicate over local data, or a precomputed permission index joined in SQL | OpenFGA "local index from ReadChanges" and "query a higher-level resource first"; AuthZed Materialize; Oso `list_local`; Zanzibar Leopard |

Sources: https://openfga.dev/docs/interacting/search-with-permissions ,
https://authzed.com/docs/spicedb/modeling/protecting-a-list-endpoint ,
https://www.osohq.com/docs/develop/enforce/list-filtering ,
https://authzed.com/docs/authzed/concepts/authzed-materialize .

Nobody answers the large case with per-object checks. AuthZed's blog says to avoid both pre- and
post-filtering at "tens of thousands or millions" of accessible objects
(https://authzed.com/blog/why-large-organizations-need-materialize).

## Engine profiles and requirements

| engine | list / filter primitive | limits and defaults | scaling requirements | notes |
|---|---|---|---|---|
| OpenFGA | ListObjects, StreamedListObjects; BatchCheck | `listObjectsMaxResults` 1000, `listObjectsDeadline` 3 s (partial result on expiry), `requestTimeout` 3 s; streaming has no result cap (changelog #733); `maxChecksPerBatchCheck` 50, `maxConcurrentChecksPerBatchCheck` 50; `resolveNodeLimit` 25, breadth 10 | Postgres or MySQL datastore (`maxOpenConns` 30 default), read replicas via `--datastore-secondary-uri` (HIGHER_CONSISTENCY goes to primary); check/iterator caches off by default (10 s TTL) and bypassed by HIGHER_CONSISTENCY; production guide favours few large servers for cache hits | ListObjects runs on a weighted-graph pipeline since v1.12; `and`/`but not` cost more; weighted-graph Check is experimental. Current release v1.22.0; the harness ran v1.15.1 until 2026-10-28 |
| SpiceDB | LookupResources (streamed, cursored: `optional_limit`, `after_result_cursor`), LookupSubjects (no cursor), CheckBulkPermissions | lookups "heavy" above ~10k results; per-call caps for CheckBulk (~10k) and `optional_limit` (~1000) (unverified) | CockroachDB/Spanner for scale, Postgres 15+ needs `track_commit_timestamp=on` for Watch, read replicas; consistency `minimize_latency` (cached) / `at_least_as_fresh` (ZedToken) / `fully_consistent`; GC window bounds snapshots and cursors | Watch API streams changes; Materialize (commercial, Dedicated only) streams precomputed permission sets |
| Zanzibar (paper) | Check; Leopard index for nested groups | Leopard: < 150 µs median, < 1 ms p99, 1.56M QPS; one tuple write can produce tens of thousands of index events | Spanner, distributed caches keyed by quantized snapshot, hedged reads | flattens group→group closure only; users stay direct members |
| OPA | partial evaluation; Compile API to SQL (postgresql, mysql, sqlserver, sqlite) and UCAST | filterable fragment: comparisons with one unknown side, a few string built-ins, no recursion, limited `not` | data in memory (~20× JSON size: 8 MB → 160 MB), ~1 ms decisions, single-threaded per query | ABAC on row attributes only; no graph traversal |
| Cedar / AVP | partial evaluation (`tpe`, experimental) leaves residual policies; no SQL generation | AVP `BatchIsAuthorized`: 30 requests per call, 30 TPS default (200 TPS for single) | caller supplies entities per request | in-process decisions in microseconds (Cedar's own benchmark) but no list primitive |
| Cerbos | PlanResources: always allowed / always denied / condition AST | adapters (Prisma, SQLAlchemy, Drizzle, ...) fail closed on untranslatable operators | stateless PDP, sidecar | attributes only, no relationship graph |
| Oso Cloud | central `list` (ids, ~10k), `list_local` (SQL WHERE over the app's own tables) | local facts mapped by config (`fact -> SELECT`); no recursion mixing central and local facts | central facts in Oso, the rest stay in the app DB | the closest model to "engine for relationships, SQL for local data" |
| Permify | LookupEntity(Stream), page size ≤ 100 | no attribute filters on lookup | Postgres with MVCC snap tokens in cache keys | no list-filtering shortcut |

## What this means for pg_auth

pg_auth already has the small-set path (listed ids) and a post-filter path (AuthzBatch). The
missing tier is the one every vendor uses for large sets: **the decision moves into Postgres**.
Three ways to get there, in order of generality:

1. **Expand inherited permissions in SQL** (OpenFGA's "query a higher-level resource first";
   Oso local facts). When a permission is inherited through a relation whose data Postgres
   already holds (a folder tree's `parent_id`, an `org_id` column), ask the engine only for the
   subject's *direct grants* (few) and expand them with a closure table or recursive query over
   the app's own rows. The hierarchical bench's broad subject has 2 direct grants and ~33k
   inherited folders: two ids plus a join instead of ~50k checks. Needs the model to say which
   engine relation corresponds to which table column (Oso's fact mapping).
2. **Materialized permission sets in Postgres** (Leopard; AuthZed Materialize; OpenFGA's local
   index from ReadChanges). A pg_auth-owned index, Leopard-shaped:
   `member_of(subject, set)` (direct), `set_closure(ancestor, descendant)` (transitive, flattened
   for deep relations only), `set_objects(set, object_type, object_id)`. Visibility becomes an
   indexed semi-join the planner handles natively. Kept fresh from the engine's change stream
   (OpenFGA `ReadChanges`, SpiceDB `Watch`), backfilled from listing APIs, with the applied
   revision recorded. Statements needing read-after-write freshness compare their token with the
   applied revision and fall back to the engine path when the index lags. Costs: write
   amplification (one tuple can fan out to tens of thousands of rows), a sync process, storage.
3. **Larger listed sets** as a stopgap: StreamedListObjects / cursored LookupResources up to
   ~10k ids (the vendors' own ceiling), with Postgres's hashed `= ANY` for large arrays.

Compiling conditions (OPA, Cedar, Cerbos) applies to attribute rules on the row itself (owner,
visibility, status, tenant). It is cheap and worth having for that part of a model, but none of
those tools traverses relationships, so it does not solve the hierarchy case alone.

**Tiering.** Pick the path per (subject, type, permission), as vendors do by result size:
listed ids up to a threshold (~1k–10k), the materialized or expanded path above it, AuthzBatch
only as the fallback while an index is stale or for models it cannot express.

**Telling the user what to change.** The thresholds above give pg_auth a principled definition
of a heavy query: a subject over the listing threshold reading many rows through AuthzBatch.
pg_auth knows, per statement, the subject's breadth, the ids checked and the engine time
(`authz.stats()`), and the model knows why a set is large (inheritance, wildcard, broad group).
Advice can name the cause and the fix: materialize this relation, expand this hierarchy in SQL
by declaring its parent column, raise `LIST_LIMIT` to N, protect by a coarser column, or add a
selective condition.

**Syntax lessons.** Declarative per-table rules limited to what can be compiled (comparisons,
`in`, `exists` over named relations, and/or), hierarchy declared explicitly rather than written
as recursion, fail closed on anything untranslatable, an explain/debug view of the generated
filter (Cerbos `filterDebug`), and a derived-column escape hatch for logic that cannot be pushed
down.

## Corrections to our benchmarks

- The numbers above came from OpenFGA v1.15.1 on the memory datastore, which is meant for
  development. The harness now runs v1.22.0 (pipeline ListObjects), and the bench defaults to a
  Postgres datastore with the check and list caches on (`compare/lib/stack.exs`); re-bench there,
  and try weighted-graph Check on and off.
- The concurrency penalty we saw for BatchCheck has no official explanation; the per-request 50
  concurrent checks across many clients contending on datastore reads is plausible (unverified).

## Open questions

- Does OpenFGA's StreamedListObjects signal a deadline expiry as an error or by ending the
  stream early? The sidecar must not mistake a cut stream for a complete one.
- Real CheckBulkPermissions and `optional_limit` caps in SpiceDB (check
  `internal/services/v1/permissions.go`).
- Write amplification of a Leopard-style index for our bench models, and how fast ReadChanges /
  Watch deltas can be applied in Postgres.
