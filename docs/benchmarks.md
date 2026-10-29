# Benchmarks

Every benchmark scenario is a row level security problem people hit in production, collected in
the [RLS issues catalogue](research/rls-issues.md). Each one runs a scenario's queries as a narrow
and a broad subject against several variants of the same schema: no RLS, RLS written naively, RLS
written the way Supabase recommends (best practice), and pg_auth enforcing OpenFGA's decisions.
pg_auth is compared with best-practice RLS.

## Method

- **Fresh containers.** Each variant gets a new Postgres 18 container on a copy of the loaded
  schema (4 CPUs, 4 GB, `shared_buffers` 1 GB), and each pg_auth variant a new OpenFGA server
  and new sidecars, removed once it is measured. Nothing one variant does reaches the next.
- **Production setups.** OpenFGA v1.22 on its Postgres datastore with its check and list caches
  on, `listObjectsMaxResults` 1000; the sidecar with its answer cache (`CACHE_TTL_MS` 1000) and
  the permission index.
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

    elixir bench/docker/run.exs --scenario all --parallel 2
    elixir bench/docker/pg_auth_table.exs bench/docker/results

Correctness against the engine is checked separately by
[`compare/parity.exs`](https://github.com/filipecabaco/pg_auth/tree/main/compare).
