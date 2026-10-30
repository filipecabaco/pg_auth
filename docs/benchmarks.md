# Benchmarks

Every benchmark scenario is a row level security problem people hit in production, collected in
the [RLS issues catalogue](research/rls-issues.md). Each one runs a scenario's queries as a narrow
and a broad subject against several variants of the same schema: no RLS, RLS written naively, RLS
written the way Supabase recommends (best practice), and pg_auth enforcing an engine's
decisions: OpenFGA and SpiceDB on every scenario, Cerbos and Cedar on the scenarios their rules
express. pg_auth is compared with best-practice RLS.

## Method

- **Fresh containers.** Each variant gets a new Postgres 18 container on a copy of the loaded
  schema (4 CPUs, 4 GB, `shared_buffers` 1 GB), and each pg_auth variant a new engine server
  and new sidecars, removed once it is measured. Nothing one variant does reaches the next.
- **Production setups.** OpenFGA v1.22 on its Postgres datastore with its check and list caches
  on, `listObjectsMaxResults` 1000. SpiceDB v1.56.2 on its Postgres datastore with its caches on,
  read at its default `minimize_latency`, spoken to through its HTTP gateway as the sidecar
  does; the scenario's OpenFGA model translated to a SpiceDB schema. The sidecar with its answer
  cache (`CACHE_TTL_MS` 1000) and the permission index. Cerbos v0.56 with its policies on disk.
  Cedar (cedar-policy 4.13) in the sidecars, its schema and policies stored with
  `authz.set_rules` and its parents loaded from the scenario's tables before the sidecars start.
- **Measured.** Planning plus execution time under `EXPLAIN (ANALYZE, BUFFERS)` as a
  non-superuser role, after `VACUUM ANALYZE` and a warmup, median of 5 runs. Writes are rolled
  back. pg_auth variants are measured in their steady state: the runner waits until no permission
  index set is filling.
- **Same rows.** Each query's result hash is compared with best-practice RLS. The three security
  scenarios that differ do so by design: their READMEs give per-path verdicts.

| variant | measures |
|---|---|
| `pg_auth` | production defaults |
| `pg_auth_per_row` | the per-row check for every subject |
| `pg_auth_reused_plan` | a cached generic plan built for the narrow subject, reused by each subject (a pooled connection's prepared statements) |
| `pg_auth_no_index` | the permission index off: broad subjects are checked with the engine |
| `pg_auth_cerbos` | Cerbos, its rules on the rows' attributes (`pg_auth/cerbos`) |
| `pg_auth_cedar` | Cedar in the sidecar, its rules on attributes and parents (`pg_auth/cedar`) |

## Results

### All engines

Each engine through pg_auth against best-practice RLS. OpenFGA and SpiceDB run every scenario;
Cerbos (rules on a row's fields) and Cedar (rules on fields and on parents, evaluated in the
sidecar) run the scenarios whose rules they express as they are written: the two attribute
scenarios, and for Cedar the organisation and folder hierarchies too. Over different sets of
scenarios the first table's figures are not comparable with each other; the second puts every
engine on the same scenarios.

<!-- results-engines:start -->
Commit `a730ca9`, 2026-10-30.

| engine | decides on | scenarios | pairs | vs best-practice RLS | faster in | same rows |
|---|---|---|---|---|---|---|
| OpenFGA | relationships | 35 | 489 | 0.79x | 190 | 486/489 |
| SpiceDB | relationships | 35 | 489 | 0.93x | 196 | 486/489 |
| Cerbos | the row's fields | 2 | 44 | 0.69x | 15 | 44/44 |
| Cedar | fields and parents | 4 | 66 | 0.79x | 20 | 66/66 |

| scenario | OpenFGA | SpiceDB | Cerbos | Cedar |
|---|---|---|---|---|
| `hierarchical-permissions` | 0.25x | 0.26x | - | 0.49x |
| `org-membership` | 1.01x | 1.48x | - | 1.01x |
| `rls-best-case-tenant-column` | 1.28x | 1.24x | 1.41x | 1.58x |
| `supabase-owner-or-role` | 0.47x | 0.95x | 0.38x | 0.46x |
<!-- results-engines:end -->

### With OpenFGA

<!-- results:start -->
Commit `a730ca9`, 2026-10-30.
Over all 489 pairs pg_auth is at 0.79x of best-practice RLS, and faster in 190 of them.

| scenario | pg_auth vs best-practice RLS | worst query | reused plan vs fresh | same rows |
|---|---|---|---|---|
| `drive-nested-group-sharing` | 0.91x | 3.04x | 0.73x | 21/21 |
| `hierarchical-permissions` | 0.25x | 1.72x | 0.84x | 8/8 |
| `join-two-rls-tables` | 1.81x | 5.07x | 0.78x | 8/8 |
| `jwt-claims-parsed-per-row` | 1.73x | 4.25x | 1.07x | 3/3 |
| `large-events-timeseries` | 0.68x | 1.52x | 0.78x | 24/24 |
| `large-fulltext-search` | 0.96x | 1.13x | 0.98x | 24/24 |
| `large-membership-array-overhead` | 0.31x | 1.58x | 0.71x | 12/12 |
| `larger-than-memory` | 0.70x | 2.74x | 0.45x | 15/15 |
| `leaky-function-and-leakproof-marking` | 1.15x | 1.50x | - | 14/14 |
| `marketplace-two-party-orders` | 0.63x | 2.34x | 0.63x | 24/24 |
| `membership-subplan-no-pullup` | 0.81x | 2.84x | 0.62x | 8/8 |
| `missing-policy-index` | 1.51x | 444.44x | 0.82x | 6/6 |
| `nested-rls-chain` | 0.20x | 1.61x | 0.77x | 10/10 |
| `non-leakproof-operators-lose-index` | 1.13x | 2.11x | 0.73x | 14/14 |
| `order-by-limit-selective-policy` | 0.24x | 2.63x | 0.63x | 9/9 |
| `org-membership` | 1.01x | 1.86x | 0.58x | 14/14 |
| `partition-direct-access` | 1.61x | 2.59x | 0.65x | 8/8 |
| `partitioned-rls-no-pruning` | 0.97x | 1.90x | 0.63x | 10/10 |
| `per-row-function-in-policy` | 1.36x | 12.71x | 0.37x | 8/8 |
| `policy-recursion-42p17` | 1.15x | 1.75x | - | 4/4 |
| `pool-session-state-leak` | 1.60x | 1.88x | - | 5/6 |
| `prepared-plan-role-revalidation` | 0.98x | 1.39x | 0.98x | 8/8 |
| `rls-best-case-tenant-column` | 1.28x | 2.05x | 0.53x | 20/20 |
| `rls-bypass-paths` | 1.08x | 1.93x | - | 6/7 |
| `rls-write-path` | 0.78x | 1.88x | 0.88x | 45/45 |
| `saas-project-hierarchy` | 0.47x | 1.43x | 0.75x | 30/30 |
| `secdef-helper-oracle` | 0.45x | 2.30x | - | 2/3 |
| `supabase-chat-rooms` | 0.95x | 2.95x | 0.56x | 21/21 |
| `supabase-claims-rbac` | 1.35x | 5.46x | 0.62x | 18/18 |
| `supabase-owner-or-role` | 0.47x | 3.03x | 0.65x | 24/24 |
| `supabase-storage-paths` | 0.35x | 3.21x | 0.49x | 21/21 |
| `tenant-skew-bad-estimates` | 1.87x | 5.97x | 0.88x | 15/15 |
| `update-delete-bulk-rls-cost` | 1.09x | 1.40x | 0.76x | 8/8 |
| `update-select-coupling` | 1.33x | 2.06x | - | 12/12 |
| `view-owner-bypass` | 0.98x | 1.70x | - | 7/7 |
<!-- results:end -->

### With SpiceDB

<!-- results-spicedb:start -->
Commit `a730ca9`, 2026-10-30.
Over all 489 pairs pg_auth is at 0.93x of best-practice RLS, and faster in 196 of them.

| scenario | pg_auth vs best-practice RLS | worst query | reused plan vs fresh | same rows |
|---|---|---|---|---|
| `drive-nested-group-sharing` | 0.82x | 2.29x | 0.69x | 21/21 |
| `hierarchical-permissions` | 0.26x | 3.83x | 0.61x | 8/8 |
| `join-two-rls-tables` | 1.82x | 3.72x | 0.82x | 8/8 |
| `jwt-claims-parsed-per-row` | 2.09x | 6.57x | 0.49x | 3/3 |
| `large-events-timeseries` | 0.73x | 1.50x | 0.67x | 24/24 |
| `large-fulltext-search` | 0.94x | 1.31x | 0.99x | 24/24 |
| `large-membership-array-overhead` | 0.33x | 3.59x | 0.63x | 12/12 |
| `larger-than-memory` | 0.69x | 2.80x | 0.82x | 15/15 |
| `leaky-function-and-leakproof-marking` | 1.48x | 1.79x | - | 14/14 |
| `marketplace-two-party-orders` | 0.67x | 1.98x | 0.79x | 24/24 |
| `membership-subplan-no-pullup` | 0.82x | 2.78x | 0.47x | 8/8 |
| `missing-policy-index` | 2.45x | 364.07x | 0.63x | 6/6 |
| `nested-rls-chain` | 0.31x | 1.09x | 0.58x | 10/10 |
| `non-leakproof-operators-lose-index` | 1.65x | 5.15x | 0.61x | 14/14 |
| `order-by-limit-selective-policy` | 0.27x | 3.00x | 0.78x | 9/9 |
| `org-membership` | 1.48x | 3.50x | 0.47x | 14/14 |
| `partition-direct-access` | 1.92x | 2.41x | 0.69x | 8/8 |
| `partitioned-rls-no-pruning` | 1.17x | 1.95x | 0.57x | 10/10 |
| `per-row-function-in-policy` | 1.01x | 3.05x | 0.66x | 8/8 |
| `policy-recursion-42p17` | 1.68x | 3.53x | - | 4/4 |
| `pool-session-state-leak` | 1.55x | 2.38x | - | 5/6 |
| `prepared-plan-role-revalidation` | 0.99x | 1.36x | 1.06x | 8/8 |
| `rls-best-case-tenant-column` | 1.24x | 3.13x | 0.61x | 20/20 |
| `rls-bypass-paths` | 1.40x | 2.53x | - | 6/7 |
| `rls-write-path` | 0.93x | 2.68x | 0.86x | 45/45 |
| `saas-project-hierarchy` | 0.52x | 2.05x | 0.97x | 30/30 |
| `secdef-helper-oracle` | 0.39x | 1.27x | - | 2/3 |
| `supabase-chat-rooms` | 2.08x | 7.11x | 0.37x | 21/21 |
| `supabase-claims-rbac` | 1.53x | 6.72x | 0.48x | 18/18 |
| `supabase-owner-or-role` | 0.95x | 6.75x | 0.41x | 24/24 |
| `supabase-storage-paths` | 0.37x | 3.44x | 0.64x | 21/21 |
| `tenant-skew-bad-estimates` | 2.61x | 10.75x | 0.48x | 15/15 |
| `update-delete-bulk-rls-cost` | 0.79x | 0.99x | 0.89x | 8/8 |
| `update-select-coupling` | 1.54x | 2.60x | - | 12/12 |
| `view-owner-bypass` | 1.42x | 3.13x | - | 7/7 |
<!-- results-spicedb:end -->

On this commit SpiceDB's figure is higher than OpenFGA's (0.93x against 0.79x) and than its own
on `79b07a5` (0.78x). An A/B of the two commits on four scenarios (`supabase-owner-or-role`,
`org-membership`, `supabase-chat-rooms`, `tenant-skew-bad-estimates`), alternated on fresh
containers, has `a730ca9` 7 to 20% slower in each of three pairings, mostly in statements under a
tenth of a millisecond whose planning took about twice as long while no engine was called; the
cause is not found yet. The same four scenarios run one at a time came out at 1.75x and 1.87x, so
on this machine a sub-millisecond statement's ratio moves with the host's load as much as with the
code: read a scenario's figure as a range.

A column is the geometric mean over the scenario's query and subject pairs; below 1x pg_auth is
faster. "Reused plan vs fresh" is `pg_auth_reused_plan` against `pg_auth`.

## Where it still loses

- **Sub-millisecond statements.** pg_auth's planning work per protected table shows on
  statements that finish in well under a millisecond: joins of two protected tables and short
  writes.
- **`missing-policy-index`** keeps its premise, no index on the column, in the pg_auth variant,
  while best-practice RLS adds the index.
- **`tenant-skew-bad-estimates`**: a tenant holding half the table, paging one kind of row.

## Reproduce

The harness is in [`bench/docker`](https://github.com/filipecabaco/pg_auth/tree/main/bench/docker):

    elixir bench/docker/run.exs --scenario all --parallel 2 --out results/openfga   # OpenFGA, Cerbos, Cedar
    elixir bench/docker/run.exs --scenario all --parallel 2 --engine spicedb --out results/spicedb \
      --variants no_rls_baseline,best_practice_rls,pg_auth,pg_auth_reused_plan
    elixir bench/docker/pg_auth_table.exs results/openfga                           # one engine's table
    elixir bench/docker/engines_table.exs results/openfga results/spicedb           # all engines

Correctness against the engine is checked separately by
[`compare/parity.exs`](https://github.com/filipecabaco/pg_auth/tree/main/compare).
