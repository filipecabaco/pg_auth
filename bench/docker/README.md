# RLS benchmark harness (Docker)

Each scenario runs in its own fresh Postgres 18 containers, so scenarios are isolated and can run in
parallel. Within a scenario every variant starts fresh too: the schema is loaded once into a seed
volume, and each variant gets a new Postgres container on a copy of it (and, for `pg_auth_*`
variants, a new OpenFGA server on the same datastore and new sidecars), removed once the variant
is measured. Nothing one variant does (dead rows, caches, settings, indexes) reaches the next, so
results do not depend on variant order. Most variants are plain RLS patterns; `pg_auth_*` variants
run pg_auth against a real OpenFGA and sidecar (see below).
For every variant the runner installs that variant's policies, runs `VACUUM ANALYZE`, warms up,
then runs each query x subject `--reps` times under `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` as a
non-superuser role (`bench_user`; superuser only for variants that declare it).

## Build the image

From the repo root (build context is the repo root; `.dockerignore` excludes `target/`):

    docker build -f bench/docker/Dockerfile -t pg_auth-bench .

The runner builds it (and the sidecar image) on every run, so results always measure the checkout;
`--no-build` reuses existing images.
First build compiles cargo-pgrx and takes several minutes; later builds reuse cached layers.

## Run

    elixir bench/docker/run.exs --scenario org-membership --scale 10000 --reps 5
    elixir bench/docker/run.exs --scenario all --parallel 2
    elixir bench/docker/run.exs --list            # scenarios and their variants
    elixir bench/docker/run.exs --clean           # remove all a run left: label pg_auth_bench=1, OpenFGA on its networks

`--sidecar-env K=V,...` passes extra environment to the sidecars (e.g. `CHECK_CONCURRENCY=16`).
`--engine spicedb` runs the `pg_auth*` variants against SpiceDB instead of OpenFGA (the scenario's
OpenFGA model translated to a SpiceDB schema; see `compare/README.md`). Variants named
`pg_auth_cerbos*` always run against Cerbos with the scenario's `pg_auth/cerbos/` policies, rules
on the rows' attributes (`supabase-owner-or-role`, `rls-best-case-tenant-column`); each engine a
scenario's variants use gets its own stack.

`--no-timing` measures with `EXPLAIN (ANALYZE, TIMING OFF)`: exact statement time, without the
per-node clock reads that inflate plans with more nodes per row (pg_auth's AuthzBatch adds one:
broad `recent_page` read 16 ms with timing, 9.9 ms without), at the cost of per-node times in the
report.

Flags: `--scenario a,b|all`, `--variants x,y`, `--queries q1,q2`, `--subjects s1`, `--reps N` (default 5,
median reported), `--scale N` (psql variable `:scale`; default from scenario.json), `--parallel N`,
`--keep` (leave the last variant's Postgres, sidecars and OpenFGA running, for a fresh stack to
experiment on; port printed is on 127.0.0.1, user postgres / password bench / db bench; `--clean`
removes it), `--cpus` (4), `--memory` (4g), `--image`, `--no-build`, `--out DIR`.

Container settings are fixed in `run.exs` (`@pg_args`): shared_buffers=1GB, work_mem=64MB,
max_parallel_workers_per_gather=0, jit=off, fsync=off, ...

Output (in `bench/docker/results/`): `<scenario>.json`, `<scenario>.md` (pivot of median ms,
per-query plan details: planning time, buffers hit/read, per-row SubPlan, partitions scanned,
row-estimate q-error at the root and worst node, top nodes by exclusive time, variant install
time, result-consistency check across variants), and `summary.md`/`summary.json` (geomean and
worst slowdown of each variant vs. the first variant, normally `no_rls_baseline`).
Results are git-ignorable run artifacts.

Note: a per-row *function* cost does not show as a SubPlan in EXPLAIN; it shows up as time in the
scan node. "per-row subplan" flags SubPlan nodes with more than one loop.

## Add a scenario

Create `bench/docker/scenarios/<id>/` (the runner discovers it automatically):

    scenario.json          metadata (see below)
    schema.sql             tables, indexes, data; run once per container via psql
    queries.sql            named queries
    variants/<name>.sql    install one variant's policies
    variants/<name>.queries.sql   optional per-variant query overrides
    README.md              the issue, expected symptom

**scenario.json**

    { "id": "...", "title": "...", "issue": "...", "symptom": "...",
      "scale": { "default": 50000, "meaning": "what :scale counts" },
      "subject_gucs": ["request.uid"],
      "subjects": [ {"name": "selective", "id": "1"}, {"name": "broad", "id": "0"} ],
      "statement_timeout": "60s",
      "vacuum_each_run": true }

`vacuum_each_run` (write scenarios): VACUUM before every measured run, since each run rolls its
write back and its dead rows would otherwise slow the runs and queries after it.

Every subject GUC listed in `subject_gucs` is set (`set_config(..., true)`) to the subject id for
each run, so policies can read `current_setting('request.uid')`. A subject with `"claims": {...}`
also gets them as `request.jwt.claims` (JSON; `sub` defaults to the subject id), as PostgREST sets
them on Supabase, so policies can call `auth.jwt()` / `auth.uid()`. pg_auth variants read the same
claims with `select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims',
claim => 'sub')`; `authz.settings` outlives a variant, so every pg_auth variant of such a
scenario calls `authz.configure` itself (`subject => 'guc'` to go back).

**schema.sql** gets `-v scale=<N>` (use `:scale`; derive other sizes with `select ... \gset`).
The `bench_user` role exists. Create your own schema (e.g. `app`);
the runner grants `bench_user` usage/DML/execute on every non-system schema.

**queries.sql**: `-- name: <q>` header, then one statement (no params). `{{subject}}` is replaced
with the subject id (useful in baseline overrides). DML is fine: every run is rolled back.

**variants/<name>.sql**: runs on a fresh copy of the loaded schema (with pg_auth's extension created
and its tuples in OpenFGA when the scenario has pg_auth variants), so a variant only *adds* its
setup. Directives in leading comments:

    -- @role: superuser            run queries as superuser (baseline) instead of bench_user
    -- @set: plan_cache_mode=force_custom_plan   GUC set for each query (repeatable)
    -- @prepare_as: selective      PREPARE each query, EXECUTE it once as that subject, then
                                   measure EXECUTE as each subject (use with force_generic_plan)

Settings (subject GUCs, then `@set`) are applied as superuser before switching to `bench_user`.

Standard names: `no_rls_baseline`, `naive_rls`, `best_practice_rls`; other names are allowed and sorted
after these. Variants a scenario does not need can simply be omitted. A
`<name>.queries.sql` overrides queries of the same name (and may add new ones); e.g. the baseline
rewrites each query with an explicit semi-join.

Smoke-test a new scenario small: `--scenario <id> --scale 1000 --reps 1`.

## pg_auth variants

A variant named `pg_auth*` runs pg_auth enforcing an engine's decisions: OpenFGA by default,
SpiceDB with `--engine spicedb`. The scenario then needs:

    pg_auth/model.fga      OpenFGA model (DSL; translated for SpiceDB)
    pg_auth/tuples.sql     one query returning (object, relation, user) rows, run after schema.sql,
                           that turns the scenario's own membership data into tuples
    "subject_gucs": [..., "pg_auth.subject_id"]   in scenario.json, so subject id N is user:N

The runner starts OpenFGA v1.22.0 as in production by default: on a migrated Postgres datastore
with its check and list caches on, `listObjectsMaxResults` 1000, ListObjects deadline 30 s; or SpiceDB v1.56.2,
also as in production: a migrated Postgres datastore, its caches on, `minimize_latency` reads.
After `schema.sql` it creates the extension and writes the tuples once. Each pg_auth variant then
gets a new engine server on that datastore and two new sidecars (`sidecar/query`, with their production
answer cache, `CACHE_TTL_MS` 1000; parity runs them with it off), on the stack's Docker network,
next to its own Postgres (reached as `bench-postgres`). A variant picks a sidecar with
`-- @set: pg_auth.sidecar_socket=/run/pg_auth/<name>.sock`:

| socket | LIST_LIMIT | |
|---|---|---|
| `default.sock` | 1000 | the sidecar's default: listed-ids plan up to 1000 objects, per-row check above |
| `per_row.sock` | 1 | never lists: every subject gets the per-row check |

Each scenario that has `pg_auth/` has these variants (most have all four):

| variant | measures |
|---|---|
| `pg_auth` | production defaults |
| `pg_auth_per_row` | the per-row check for every subject |
| `pg_auth_reused_plan` | a cached generic plan built for the selective subject, reused by each subject (a pooled connection's prepared statements) |
| `pg_auth_no_index` | production defaults with `pg_auth.permission_index = off`: broad subjects are checked with the engine |

The sidecars keep the permission index in the scenario's Postgres (`PG_AUTH_DATABASE_URL`). A
warmup run may ask them to fill sets; the runner waits until no set is filling before measuring,
so pg_auth variants are measured in their steady state.

For these variants the runner reads `authz.stats()` around every measured statement, and the
report adds a "pg_auth round trips" table: planning plus execution time split into Postgres,
round trips to the sidecar, and the part of those the sidecar spent waiting on OpenFGA, with the
number of sidecar calls and ids checked. OpenFGA and the sidecars are not limited by `--cpus`.

## Results (2026-10-29)

Commit `eb69ff7`, every scenario, default scales, 5 reps (median), two scenarios at a time,
every variant on fresh containers, OpenFGA on its Postgres datastore with caches on. Times are
planning plus execution, per query and subject; a column is the geometric mean over the
scenario's pairs (below 1x: pg_auth is faster). "Reused plan vs fresh" is `pg_auth_reused_plan`
against `pg_auth`. "Same rows" compares result hashes with
best-practice RLS: the three security scenarios that differ do so by design (their READMEs give
per-path verdicts). All from one run of one commit; reproduce with
`elixir bench/docker/pg_auth_table.exs <results dir>`, and publish it here, in the RLS issues
catalogue and on the site with `bench/docker/apply_zero.exs`. Numbers inside a scenario's README come from
the run they are dated with; this table is the current baseline.

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

Where it still loses: `missing-policy-index` keeps its premise (no index on the column) in the
pg_auth variant while best-practice RLS adds the index; pg_auth's planning work per protected
table shows on sub-millisecond statements (joins of two protected tables, short writes); and
`tenant-skew-bad-estimates` (a tenant with half the table paging one kind of row) is open.
