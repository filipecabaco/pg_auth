# Benchmarks

Every benchmark scenario is a row level security problem people hit in production, collected in
the [RLS issues catalogue](research/rls-issues.md). Each one runs a scenario's queries as a narrow
and a broad subject against several variants of the same schema: no RLS, RLS written naively, RLS
written the way Supabase recommends (best practice), and pg_auth enforcing an engine's
decisions, once with OpenFGA and once with SpiceDB. pg_auth is compared with best-practice RLS.

## Method

- **Fresh containers.** Each variant gets a new Postgres 18 container on a copy of the loaded
  schema (4 CPUs, 4 GB, `shared_buffers` 1 GB), and each pg_auth variant a new engine server
  and new sidecars, removed once it is measured. Nothing one variant does reaches the next.
- **Production setups.** OpenFGA v1.22 on its Postgres datastore with its check and list caches
  on, `listObjectsMaxResults` 1000. SpiceDB v1.56.2 on its Postgres datastore with its caches on,
  read at its default `minimize_latency`, spoken to through its HTTP gateway as the sidecar
  does; the scenario's OpenFGA model translated to a SpiceDB schema. The sidecar with its answer
  cache (`CACHE_TTL_MS` 1000) and the permission index. Variants on Cerbos (attribute rules) run
  against Cerbos in both.
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

## Results

### With OpenFGA

<!-- results:start -->
Commit `eb69ff7`, 2026-10-29.
Over all 489 pairs pg_auth is at 0.80x of best-practice RLS, and faster in 201 of them.

| scenario | pg_auth vs best-practice RLS | worst query | reused plan vs fresh | same rows |
|---|---|---|---|---|
| `drive-nested-group-sharing` | 0.92x | 1.78x | 0.61x | 21/21 |
| `hierarchical-permissions` | 0.21x | 1.12x | 0.79x | 8/8 |
| `join-two-rls-tables` | 1.23x | 2.40x | 1.02x | 8/8 |
| `jwt-claims-parsed-per-row` | 0.90x | 1.28x | 1.19x | 3/3 |
| `large-events-timeseries` | 0.71x | 1.46x | 0.71x | 24/24 |
| `large-fulltext-search` | 0.98x | 1.12x | 0.98x | 24/24 |
| `large-membership-array-overhead` | 0.39x | 4.16x | 0.64x | 12/12 |
| `larger-than-memory` | 0.79x | 2.78x | 0.82x | 15/15 |
| `leaky-function-and-leakproof-marking` | 1.17x | 1.43x | - | 14/14 |
| `marketplace-two-party-orders` | 0.59x | 1.58x | 0.70x | 24/24 |
| `membership-subplan-no-pullup` | 0.87x | 2.86x | 0.64x | 8/8 |
| `missing-policy-index` | 1.70x | 442.88x | 0.86x | 6/6 |
| `nested-rls-chain` | 0.22x | 1.32x | 0.82x | 10/10 |
| `non-leakproof-operators-lose-index` | 1.05x | 2.08x | 0.66x | 14/14 |
| `order-by-limit-selective-policy` | 0.24x | 2.71x | 0.59x | 9/9 |
| `org-membership` | 1.07x | 2.74x | 0.55x | 14/14 |
| `partition-direct-access` | 1.64x | 2.15x | 0.58x | 8/8 |
| `partitioned-rls-no-pruning` | 0.94x | 1.87x | 0.61x | 10/10 |
| `per-row-function-in-policy` | 1.06x | 2.75x | 0.58x | 8/8 |
| `policy-recursion-42p17` | 1.06x | 1.81x | - | 4/4 |
| `pool-session-state-leak` | 1.56x | 2.30x | - | 5/6 |
| `prepared-plan-role-revalidation` | 0.97x | 1.17x | 0.97x | 8/8 |
| `rls-best-case-tenant-column` | 1.38x | 3.05x | 0.52x | 20/20 |
| `rls-bypass-paths` | 1.06x | 1.94x | - | 6/7 |
| `rls-write-path` | 0.85x | 2.37x | 0.92x | 45/45 |
| `saas-project-hierarchy` | 0.54x | 2.18x | 0.66x | 30/30 |
| `secdef-helper-oracle` | 0.38x | 1.41x | - | 2/3 |
| `supabase-chat-rooms` | 0.93x | 3.28x | 0.51x | 21/21 |
| `supabase-claims-rbac` | 1.32x | 4.82x | 0.76x | 18/18 |
| `supabase-owner-or-role` | 0.46x | 3.14x | 0.59x | 24/24 |
| `supabase-storage-paths` | 0.34x | 3.38x | 0.50x | 21/21 |
| `tenant-skew-bad-estimates` | 1.68x | 6.39x | 0.80x | 15/15 |
| `update-delete-bulk-rls-cost` | 1.04x | 1.92x | 0.92x | 8/8 |
| `update-select-coupling` | 1.60x | 2.87x | - | 12/12 |
| `view-owner-bypass` | 1.33x | 2.87x | - | 7/7 |
<!-- results:end -->

### With SpiceDB

<!-- results-spicedb:start -->
Commit `79b07a5`, 2026-10-29.
Over all 489 pairs pg_auth is at 0.78x of best-practice RLS, and faster in 192 of them.

| scenario | pg_auth vs best-practice RLS | worst query | reused plan vs fresh | same rows |
|---|---|---|---|---|
| `drive-nested-group-sharing` | 1.41x | 4.05x | 0.45x | 21/21 |
| `hierarchical-permissions` | 0.17x | 0.87x | 0.91x | 8/8 |
| `join-two-rls-tables` | 1.98x | 4.38x | 0.72x | 8/8 |
| `jwt-claims-parsed-per-row` | 1.46x | 2.39x | 0.68x | 3/3 |
| `large-events-timeseries` | 0.70x | 1.56x | 0.72x | 24/24 |
| `large-fulltext-search` | 1.00x | 1.37x | 0.95x | 24/24 |
| `large-membership-array-overhead` | 0.45x | 3.14x | 0.57x | 12/12 |
| `larger-than-memory` | 0.68x | 2.80x | 0.81x | 15/15 |
| `leaky-function-and-leakproof-marking` | 1.12x | 1.88x | - | 14/14 |
| `marketplace-two-party-orders` | 0.61x | 1.47x | 0.71x | 24/24 |
| `membership-subplan-no-pullup` | 0.86x | 2.86x | 0.61x | 8/8 |
| `missing-policy-index` | 1.87x | 347.15x | 0.71x | 6/6 |
| `nested-rls-chain` | 0.22x | 1.56x | 0.77x | 10/10 |
| `non-leakproof-operators-lose-index` | 0.98x | 1.87x | 0.75x | 14/14 |
| `order-by-limit-selective-policy` | 0.22x | 2.46x | 0.58x | 9/9 |
| `org-membership` | 0.77x | 1.57x | 0.68x | 14/14 |
| `partition-direct-access` | 1.51x | 2.58x | 0.62x | 8/8 |
| `partitioned-rls-no-pruning` | 1.01x | 1.92x | 0.64x | 10/10 |
| `per-row-function-in-policy` | 0.77x | 2.69x | 1.03x | 8/8 |
| `policy-recursion-42p17` | 1.23x | 2.00x | - | 4/4 |
| `pool-session-state-leak` | 1.53x | 1.89x | - | 5/6 |
| `prepared-plan-role-revalidation` | 0.98x | 1.36x | 1.02x | 8/8 |
| `rls-best-case-tenant-column` | 1.25x | 2.04x | 0.55x | 20/20 |
| `rls-bypass-paths` | 1.05x | 2.04x | - | 6/7 |
| `rls-write-path` | 0.81x | 1.87x | 0.78x | 45/45 |
| `saas-project-hierarchy` | 0.45x | 1.32x | 0.71x | 30/30 |
| `secdef-helper-oracle` | 0.37x | 1.48x | - | 2/3 |
| `supabase-chat-rooms` | 0.96x | 3.23x | 0.56x | 21/21 |
| `supabase-claims-rbac` | 1.26x | 5.35x | 0.62x | 18/18 |
| `supabase-owner-or-role` | 0.44x | 4.86x | 0.54x | 24/24 |
| `supabase-storage-paths` | 0.31x | 2.99x | 0.52x | 21/21 |
| `tenant-skew-bad-estimates` | 1.58x | 5.83x | 0.77x | 15/15 |
| `update-delete-bulk-rls-cost` | 0.98x | 1.23x | 0.86x | 8/8 |
| `update-select-coupling` | 1.41x | 1.94x | - | 12/12 |
| `view-owner-bypass` | 1.25x | 1.80x | - | 7/7 |
<!-- results-spicedb:end -->

pg_auth's own times on SpiceDB are close to its times on OpenFGA (`jwt-claims-parsed-per-row`'s
count: 7.7 ms against 8.1 ms); where a scenario's ratio differs between the two tables, it is
mostly the best-practice policy's own sub-millisecond times moving between runs (the same
selective join: 0.16 ms in one run, 0.28 ms in the other). Four scenarios
(`drive-nested-group-sharing`, `hierarchical-permissions`, `join-two-rls-tables`,
`jwt-claims-parsed-per-row`) were measured again on fresh containers; the table has the second
measurement.

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

    elixir bench/docker/run.exs --scenario all --parallel 2                    # OpenFGA
    elixir bench/docker/run.exs --scenario all --parallel 2 --engine spicedb
    elixir bench/docker/pg_auth_table.exs bench/docker/results

Correctness against the engine is checked separately by
[`compare/parity.exs`](https://github.com/filipecabaco/pg_auth/tree/main/compare).
