# Performance

pg_auth asks an engine outside Postgres who may see what, and still competes with row level
security written by hand and tuned: over the 489 query and subject pairs of the
[benchmarks](benchmarks.md) (35 scenarios, 2026-10-29), it takes 0.75x the time of best-practice
RLS, planning and execution together, and is faster in 225 of them. How the pieces work is in
[the implementation](design.md); this page is what they cost and how to tune them.

## Where it wins and where it loses

Faster than hand-tuned RLS where a policy has to do real work:

| scenario | pg_auth vs best-practice RLS |
|---|---|
| permissions inherited down a folder tree | 0.22x |
| a policy reading another protected table | 0.23x |
| a selective subject paging an ordered table | 0.24x |
| storage paths (a key expression) | 0.28x |
| a large membership array | 0.40x |
| Supabase owner-or-role | 0.45x |

Slower where a policy is a single indexed comparison, which is RLS's best case:

| scenario | pg_auth vs best-practice RLS |
|---|---|
| one tenant holding half the table, paging one kind of row | 1.7x |
| joining two protected tables | 1.6x |
| reading one partition directly | 1.5x |
| `count(*)` over rows filtered on JWT claims | 1.3x |
| a tenant column, RLS's best case | 1.3x |

On such statements, which run in under 0.1 ms, pg_auth's planning (about 0.02 ms per protected
table, warm) is a visible share of the total. Run-to-run variation per scenario is large, so a
change is judged by building both versions and running them side by side, alternately, not by
comparing with a published table ([benchmarks](benchmarks.md)).

## What costs what

- **Planning.** The planner hook asks the sidecar for the subject's answer (an id list, a plan,
  or "too many") and rewrites the policy into a filter the planner can index. Answers are kept per
  backend for half the sidecar's cache TTL, and the sidecar keeps the answers of recently active
  subjects fresh in the background, so planning rarely waits on the engine: after 1.5 s idle,
  0.47 ms (2.65 ms before the background refresh), most of it the host's cold caches.
- **A connection's first request.** Loading the configuration, preparing pg_auth's queries and
  connecting to the sidecar: `authz.warm()` does it ahead of time (first planning 0.4 ms ->
  0.15 ms). Call it as a pooler's connect query.
- **Broad subjects.** A subject who may see more objects than the engine lists has its rows
  checked in batches (one sidecar request per batch, growing to 1000 ids), and then from the
  permission index, a copy of its set kept in Postgres and updated from the engine's change feed:
  no engine calls at all in steady state.
- **Attribute rules (Cerbos).** The subject's plan is compiled into a filter on the row's
  columns; its values are read at execution, so one cached plan serves every principal whose plan
  has the same shape.
- **Writes.** Updates and deletes by a broad subject find their rows through the permission
  index; single-row writes are one check.

## Tuning

| what | how |
|---|---|
| index the id column | as for RLS; `authz.doctor()` names the missing ones |
| warm connections | `select authz.warm()` as the pooler's connect query |
| answer freshness against engine load | the sidecar's `CACHE_TTL_MS` (default 1 s) and `REFRESH_ACTIVE_MS` (default 30 s) |
| broad subjects | `pg_auth.permission_index` (on) with the sidecar's `PG_AUTH_DATABASE_URL`; `pg_auth.fill_wait_ms`, `pg_auth.fill_wait_after` |
| cached plans shared by subjects of different breadth | `pg_auth.replan_on_breadth` (on) rebuilds them |
| statements that spend time in the engine | `pg_auth.warn_ms`, `authz.last_statement_advice()`, `authz.explain(sql)` |

## Measuring

`bench/docker/run.exs` runs the benchmark scenarios on fresh containers, and
`bench/docker/planning.exs` times one query's planning after idle gaps of different lengths, with
the sidecar calls and engine time each statement caused. Profiling a backend or the sidecar with
samply is in [development.md](development.md).
