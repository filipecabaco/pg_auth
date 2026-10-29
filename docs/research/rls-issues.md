# Catalog of real-world Postgres RLS problems (benchmark scenarios for pg_auth)

Status: draft · 2026-10-27 · Postgres 18.6 (Homebrew, aarch64)

Purpose: a list of RLS problems that people actually hit, each with a reproducible scenario, so
that every one can become a benchmark case. Background: [`why-rls-is-slow.md`](why-rls-is-slow.md) and
[`scaling-db-native.md`](scaling-db-native.md). That survey is not repeated here.

How to read this file:
- **Evidence** links were opened or returned by search while writing this catalog. Anything I
  could not confirm is marked **UNVERIFIED**. Postgres source references are from memory of
  REL_17/18 and are marked **(src, unverified line)** where I did not re-open the file.
- Every repro was **actually run** on a scratch PG 18.6 cluster at `scale=1` (200k documents,
  2k orgs). The "Measured" line quotes those runs: single runs on a warm cache, parallel query
  off, an indication of shape and not a benchmark. Run at `scale=5..10` for clearer ratios.
- All scenarios share one fixture (Appendix A, `fixture.sql`). Each scenario starts with
  `SELECT bench.reset();` which drops all policies and disables RLS on the `bench` tables. Queries
  run as `SET ROLE rls_app` (NOBYPASSRLS) with the subject in `app.uid`.
- Subjects: **user 1 = small** (2 oldest orgs, 200 docs), **user 2 = power** (newest half of the
  orgs, 100k docs), user 9999 appears in scenarios that need it.
- Scenario SQL blocks are snippets to run after the fixture, in one psql session, e.g.
  `psql -v scale=1 -f fixture.sql -f scenario.sql`. Timing: `\timing on` and
  `EXPLAIN (ANALYZE, BUFFERS)`.
- Some scenarios add tables (`documents_p`, `events`, folders). They are self-contained and
  idempotent, but `partition-direct-access` needs the table from `partitioned-rls-no-pruning`.

## Summary (ranked by how common x how severe)

"pg_auth (bench)" column: pg_auth's time against the best plain-RLS variant in
[the benchmarks](../benchmarks.md) (geometric mean over the scenario's queries and
subjects, 2026-10-29; below 1x is faster), every variant on fresh containers. Correctness issues
have per-path verdicts in their scenario README instead.

The **pg_auth:** notes under each issue were written for an earlier design that precomputed
permissions inside Postgres: *mode A* was a policy over an array of the subject's objects, *mode B*
a join against the precomputed `authz.effective` table. That engine was removed; the external
engine decides. Read those notes as the problem each issue poses. Today pg_auth answers with listed
ids (`col = ANY(the engine's list)`, indexable), AuthzBatch (rows checked in batches, or against
the subject's permission index set), declared hierarchies, and estimates and cached plans that
follow the subject ([design.md](../design.md)).

| # | id | cat | one-line | measured here (naive vs best plain-RLS) | pg_auth (bench) |
|---|---|---|---|---|---|
| 1 | `per-row-function-in-policy` | perf | plpgsql / SECURITY DEFINER helper called per row | 302-338 ms vs 0.5 ms (count, small user) | 1.06x |
| 2 | `jwt-claims-parsed-per-row` | perf | `current_setting('request.jwt.claims')::jsonb` per row | 188 ms vs 6 ms | 0.90x |
| 3 | `membership-subplan-no-pullup` | perf | EXISTS / IN on membership table stays a SubPlan | 12.8 ms vs 0.5 ms count; 19 ms vs 0.08 ms latest-20 (power) | 0.87x |
| 4 | `missing-policy-index` | perf | no index on the column the policy filters | 18.5 ms vs <1 ms | 1.70x (its pg_auth variant keeps the index missing) |
| 5 | `view-owner-bypass` | correctness | view owned by superuser ignores RLS | 200000 rows visible instead of 200 | verdicts in the scenario README |
| 6 | `nested-rls-chain` | perf | policy on A reads B which has its own RLS | 22 ms vs 1.3 ms | 0.22x |
| 7 | `non-leakproof-operators-lose-index` | perf | LIKE / ILIKE / jsonb / lower() lose the user's index | 10.5 ms vs 0.12 ms (power user) | 1.05x |
| 8 | `order-by-limit-selective-policy` | perf | planner walks the sort index and filters | 13 ms (199,800 rows filtered) vs 0.08 ms | 0.24x |
| 9 | `update-select-coupling` | correctness | UPDATE needs SELECT policy; new row must pass it | silent 0-row UPDATE; soft-delete rejected | verdicts in the scenario README |
| 10 | `pool-session-state-leak` | correctness | GUC identity leaks or vanishes across pooled txns | cross-tenant read; `''::int` errors | verdicts in the scenario README |
| 11 | `policy-recursion-42p17` | correctness | policy reads its own table | ERROR 42P17 | verdicts in the scenario README |
| 12 | `secdef-helper-oracle` | correctness | SECURITY DEFINER helper taking a user id | enumerates other users' memberships | verdicts in the scenario README |
| 13 | `rls-bypass-paths` | correctness | owner / BYPASSRLS / matview / pg_dump / replication | owner sees 200000 rows | verdicts in the scenario README |
| 14 | `partition-direct-access` | correctness | leaf partitions queried directly skip the parent policy | 14300 rows instead of 0 | verdicts in the scenario README |
| 15 | `partitioned-rls-no-pruning` | perf | policy array does not prune partitions | 16/16 partitions scanned, 11 ms | 0.94x |
| 16 | `join-two-rls-tables` | perf | two RLS tables joined, SubPlans on both sides | est. 50000 rows vs 200 actual; 10.3 ms vs 0.2 ms | 1.23x |
| 17 | `tenant-skew-bad-estimates` | perf | `tenant_id = (select setting)` hides the value from stats | est. 100 vs 100000 rows | 1.68x |
| 18 | `hierarchical-permissions` | perf | recursive CTE or ancestor walk inside a policy | 333 ms vs 0.5 ms (100k files) | 0.21x (declared hierarchy: same warm; 22 ms vs 6.4 s cold) |
| 19 | `large-membership-array-overhead` | perf | `= ANY(array)` rebuilds a huge array per statement | PK lookup 0.2 ms vs 16 ms | 0.39x |
| 20 | `leaky-function-and-leakproof-marking` | correctness | LEAKPROOF used as a perf hack leaks hidden rows | 201 hidden-row values observed | verdicts in the scenario README |
| 21 | `update-delete-bulk-rls-cost` | perf | USING + WITH CHECK evaluated per written row | 2.9 s vs 2.0 s (100k-row UPDATE) | 1.04x |
| 22 | `prepared-plan-role-revalidation` | perf | plan cache is tied to the role; generic plans | +12-15% per call (trivial query) | 0.97x |

Ranking rationale: 1-4 are the bulk of "my RLS is slow" threads. 5 and 9-10 are the most common
security and "why do I get 0 rows" reports. 6-8 and 15-19 matter most for an authz engine because
they are where flat-join plans beat any static policy shape. 20 is an invariant: user conditions
that are not leakproof must only ever see rows the check allowed.

---

## 1. `per-row-function-in-policy` — helper function evaluated for every row

**Category:** perf · **Rank:** 1

**Evidence**
- Supabase docs, "Row Level Security" (performance section: wrap helpers in `select`, index policy
  columns, `to` clause): <https://supabase.com/docs/guides/database/postgres/row-level-security>
  (opened).
- Supabase troubleshooting "RLS Performance and Best Practices":
  <https://supabase.com/docs/guides/troubleshooting/rls-performance-and-best-practices-Z5Jjwv>
  (via search). Quoted figures, repeated by third parties: `auth.uid()` wrap 179 ms to 9 ms,
  security definer role check 178,000 ms to 12 ms, join flipped 9,000 ms to 20 ms. The figures
  come from Supabase's own test tables, so treat them as UNVERIFIED by us.
- pgsql-performance thread "Performance issue when we use policies for Row Level Security along
  with functions" (seq scan, 88,930 rows removed by filter, ~1.8 s):
  <https://www.postgresql.org/message-id/BL0PR11MB3153082AF1A7B16DEE74DCE5EE210%40BL0PR11MB3153.namprd11.prod.outlook.com>
  (via search, thread not read in full).

**Symptom:** a simple `count(*)` or list query takes seconds; EXPLAIN shows a `Seq Scan` with
`Filter: is_member(org_id)` and a huge `Rows Removed by Filter`.

**Root cause:** a policy qual is an ordinary expression evaluated per row. A function that takes
a row column cannot be hoisted. PL/pgSQL functions are never inlined, and neither are
`SECURITY DEFINER` or `SET`-clause functions (src: `inline_function` in `clauses.c`, unverified
line). Each call starts an executor for the inner query. Only functions with no row-dependent
arguments can be wrapped as `(select f())`, which makes an InitPlan evaluated once.

**Naive policy**
```sql
CREATE POLICY p ON bench.documents FOR SELECT TO rls_app USING (bench.is_member_plpgsql(org_id));
```

**Best-practice fix:** turn the row-dependent function into a once-per-statement set:
`org_id = ANY (ARRAY(SELECT bench.my_orgs()))` (the shape the earlier design called mode A).
`IN (SELECT my_orgs())` is better than the function but still a hashed SubPlan.

**Repro** (after fixture)
```sql
SET max_parallel_workers_per_gather = 0;
\timing on
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY p ON bench.documents FOR SELECT TO rls_app USING (bench.is_member_plpgsql(org_id));
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.documents;                       -- naive (also try is_member_secdef)
RESET ROLE; DROP POLICY p ON bench.documents;
CREATE POLICY p ON bench.documents FOR SELECT TO rls_app
  USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));       -- fix
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.documents;
EXPLAIN (ANALYZE, BUFFERS) SELECT count(*) FROM bench.documents;
```

**Measure:** execution time, `Rows Removed by Filter`, function call count
(`track_functions = 'all'`, `pg_stat_user_functions`), presence of `InitPlan` vs `SubPlan`.

**Measured (scale 1):** plpgsql helper 338 ms; SQL SECURITY DEFINER helper 302 ms;
`IN (SELECT my_orgs())` 5.9 ms; `= ANY(ARRAY(...))` 0.5 ms. Ratio grows linearly with row count.

**pg_auth:** **Y.** Mode A emits the array form directly; mode B removes even the array.

---

## 2. `jwt-claims-parsed-per-row` — `current_setting('request.jwt.claims')::jsonb` in the policy

**Category:** perf · **Rank:** 2

**Evidence**
- PostgREST docs issue #609 (moved from PostgREST #2590), "current_setting can lead to bad
  performance when used on RLS": 3M rows, `count(*)` 18.6 s with full claims, 3.4 s with short
  claims, 125 ms with an index, 264 ms with the `(select ...)` InitPlan:
  <https://github.com/PostgREST/postgrest-docs/issues/609> and
  <https://github.com/PostgREST/postgrest/issues/2590> (opened).
- Supabase RLS performance docs (wrap `auth.jwt()` / `auth.uid()` in `select`), URLs under issue 1.

**Symptom:** time scales with the size of the JWT claims; every row parses the JSON.

**Root cause:** `current_setting()` is STABLE, so it is evaluated per row in a filter. When no
index is usable, the whole `::jsonb` cast and `->>` run per row. Note: the thread's maintainers
point out this is **not** a leakproofness effect (the function sits inside the policy, where
leakproofness is irrelevant); an equivalent non-RLS query is equally slow. When an index exists,
the stable expression is evaluated once as an index condition and the problem disappears.

**Naive policy**
```sql
USING (owner_id = (current_setting('request.jwt.claims', true)::jsonb ->> 'sub')::int)
```

**Best-practice fix:** wrap in a scalar subquery so the planner builds an InitPlan, and index the
column. Alternatives that appeared in the thread: short legacy GUCs, a pre-request function,
`PARALLEL RESTRICTED`, a materialized CTE.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY naive ON bench.documents FOR SELECT TO rls_app
  USING (owner_id = (current_setting('request.jwt.claims', true)::jsonb ->> 'sub')::int);
SET ROLE rls_app;
SELECT set_config('request.jwt.claims',
  jsonb_build_object('sub','5','role','authenticated','pad',repeat('x',2000))::text, false) AS _ \gset
\timing on
SELECT count(*) FROM bench.documents;          -- no index on owner_id: per-row jsonb parse
RESET ROLE;
DROP POLICY naive ON bench.documents;
CREATE POLICY fixed ON bench.documents FOR SELECT TO rls_app
  USING (owner_id = (SELECT (current_setting('request.jwt.claims', true)::jsonb ->> 'sub')::int));
SET ROLE rls_app;
SELECT set_config('request.jwt.claims',
  jsonb_build_object('sub','5','role','authenticated','pad',repeat('x',2000))::text, false) AS _ \gset
SELECT count(*) FROM bench.documents;
-- control: CREATE INDEX ON bench.documents(owner_id); naive policy is then a 0.1 ms index scan
```

**Measure:** execution time vs claim size (vary `repeat('x', N)`), plan: `Filter:` vs `InitPlan`.

**Measured (scale 1, 2 KB claims, no index):** naive 188 ms; InitPlan 6.4 ms. With an index on
`owner_id` the naive form is already an index-only scan at 0.09 ms.

**pg_auth:** **Y.** Identity is resolved once per statement (subject lookup), and the policy
becomes a lookup in `authz.effective`. Parsing happens at session/txn start, not per row.

---

## 3. `membership-subplan-no-pullup` — EXISTS / IN on a membership table

**Category:** perf · **Rank:** 3

**Evidence**
- Supabase community discussion on joins in `USING` ("silent killer"), with `team_id =
  ANY(auth.user_team_ids())` suggested: <https://github.com/orgs/community/discussions/189420>
  (via search; the discussion lives in the GitHub community org, and I did not read it fully).
- Supabase docs recommend `... in (select ...)` over joins ("Rewriting a join so the row's
  column is compared to a set"), 9,000 ms to 20 ms in their benchmark (UNVERIFIED by us).
- pgsql-performance "Why is query performance on RLS enabled Postgres worse?" (seq scan with a
  hashed SubPlan filter removing ~4.2M rows to return 14, policy on `current_setting()`):
  <https://www.postgresql.org/message-id/CAPcM0QW3yFGqnRGvR2PzbeZVYBuQTkE72%2Brdt9dE6Bqs2KSwgQ%40mail.gmail.com>
  (via search, UNVERIFIED beyond the snippet).
- Root-cause design note: why-rls-is-slow.md section 1 (sublinks in `securityQuals` are not pulled up).

**Symptom:** `EXPLAIN` shows `Filter: (ANY (org_id = (hashed SubPlan 1).col1))` on a seq scan or
full index scan. Cost is O(table), not O(rows the subject can see). Also `count(*)` and
`count: exact` pagination over the table are O(table).

**Root cause:** `pull_up_sublinks` only runs over the join tree (`jointree->quals`), not
`rte->securityQuals` (src: `subquery_planner`, `prepjointree.c`, unverified line). The
membership lookup therefore stays a (hashed) SubPlan: no join reordering, no parameterized
nested loop, no partition pruning from the join, and no good row estimate.

**Naive policy**
```sql
USING (EXISTS (SELECT 1 FROM bench.memberships m
               WHERE m.org_id = documents.org_id AND m.user_id = bench.uid()))
```

**Best-practice fix:** `org_id = ANY (ARRAY(SELECT my_orgs()))` (indexable InitPlan array).
Still 30-260x slower than a hand-written join for selective subjects (why-rls-is-slow.md section 2).

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE bench.memberships ENABLE ROW LEVEL SECURITY;
CREATE POLICY m ON bench.memberships FOR SELECT TO rls_app USING (user_id = bench.uid());
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app
  USING (EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = documents.org_id AND m.user_id = bench.uid()));
SET ROLE rls_app; SET app.uid = 1;
EXPLAIN (ANALYZE, BUFFERS) SELECT count(*) FROM bench.documents;
SET app.uid = 2;                                                -- power user
EXPLAIN (ANALYZE, BUFFERS) SELECT * FROM bench.documents ORDER BY created_at DESC LIMIT 20;
-- compare: swap in  USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())))
```

**Measure:** execution time and buffers for `count(*)`, `SELECT * LIMIT 20`, PK lookup; plan node
(`SubPlan` vs `InitPlan`); `Rows Removed by Filter`.

**Measured (scale 1):** small user `count(*)`: EXISTS 12.8 ms, IN 8.2 ms, array 0.5 ms. Power
user latest-20: SubPlan 19 ms, array 0.08 ms. (Power-user ordering is reversed in
`order-by-limit-selective-policy`.)

**pg_auth:** **Y.** This is the primary target of mode B (semi-join on `authz.effective`).

---

## 4. `missing-policy-index` — no index on the column the policy filters

**Category:** perf · **Rank:** 4

**Evidence**
- Supabase docs: "Index every column your policies filter on", with the note that a column counts
  only if it leads a composite index (a `(team_id, user_id)` PK does not index `user_id`):
  <https://supabase.com/docs/guides/database/postgres/row-level-security> (opened). Quoted
  benchmark 171 ms to <0.1 ms (via third parties, UNVERIFIED).
- PostgREST #609 thread above: "plain policy with an index: ~125 ms" vs 18.6 s without.

**Symptom:** seq scan with a filter on a policy column; fine in dev, bad in prod.

**Root cause:** ordinary missing index. The RLS-specific part: the policy column is not in
any query the developer wrote, so nobody notices it in the application's query review.

**Naive policy:** any, with no supporting index (e.g. `org_id = ANY(...)`, no `org_id` index).

**Best-practice fix:** btree index on every column used by a policy, with the tenant/org column
leading in composite indexes. Check with `EXPLAIN` as the real role, not the owner.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
DROP INDEX bench.documents_org_idx;
SET ROLE rls_app; SET app.uid = 1;
\timing on
SELECT count(*) FROM bench.documents;           -- seq scan
RESET ROLE;
CREATE INDEX documents_org_idx ON bench.documents (org_id);   -- restore
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.documents;
```

**Measure:** time and plan node; run for each policy-referenced column. A catalog query
(`pg_policy` -> referenced columns vs `pg_index`) can lint this.

**Measured (scale 1):** 18.5 ms (seq scan) vs sub-millisecond (index).

**pg_auth:** **N** as a fix (it is schema hygiene), but `authz.effective` is created with the
indexes the rewrite needs, and the extension can offer a lint view.

---

## 5. `view-owner-bypass` — views ignore RLS when owned by a privileged role

**Category:** correctness · **Rank:** 5

**Evidence**
- PostgreSQL docs, CREATE VIEW (`security_invoker`, PG15+):
  <https://www.postgresql.org/docs/current/sql-createview.html> (via search).
- Supabase discussion "Row level security on views" (#1501):
  <https://github.com/orgs/supabase/discussions/1501> (via search).
- PostgREST discussion #1750 ("RLS and views"):
  <https://github.com/PostgREST/postgrest/discussions/1750> (via search).
- Write-ups: <https://www.bytebase.com/blog/postgres-row-level-security-footguns/> and
  <https://dev.to/udlxix/why-a-postgres-view-can-silently-bypass-row-level-security-2c2a>
  (via search). pganalyze "5mins E28" covers security_invoker and leakproof:
  <https://pganalyze.com/blog/5mins-postgres-row-level-security-bypassrls-security-invoker-views-leakproof-functions>.

**Symptom:** the API role sees all tenants' rows through a view that "obviously" selects from a
protected table. No error, no warning.

**Root cause:** a view's base tables are accessed with the view **owner's** privileges and RLS
status. Owner = superuser / BYPASSRLS / table owner without FORCE => policies are skipped.
`security_invoker = true` (PG15+) switches the check to the invoking user. `security_barrier`
is a different property (it controls qual push-down, not whose identity is used).

**Naive:** `CREATE VIEW v AS SELECT ... FROM protected_table;` executed by the migration role.

**Best-practice fix:** `WITH (security_invoker = true)` on every view over RLS tables (PG15+),
or own the view with a NOBYPASSRLS role; revoke direct grants on base tables if needed.

**Repro**
```sql
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
CREATE OR REPLACE VIEW bench.documents_v  AS SELECT id, org_id, title FROM bench.documents;
CREATE OR REPLACE VIEW bench.documents_vi WITH (security_invoker = true) AS SELECT id, org_id, title FROM bench.documents;
GRANT SELECT ON bench.documents_v, bench.documents_vi TO rls_app;
SET ROLE rls_app; SET app.uid = 1;
SELECT 'base table' AS src, count(*) FROM bench.documents
UNION ALL SELECT 'view (owner = superuser)', count(*) FROM bench.documents_v
UNION ALL SELECT 'view security_invoker', count(*) FROM bench.documents_vi;
```

**Measure:** row counts per path (a correctness assertion, not a latency). Add a catalog
check: views in exposed schemas over RLS tables without `security_invoker`.

**Measured:** base 200, owner-view 200000, security_invoker view 200.

**pg_auth:** **P.** Extension can detect/lint, and its post-analyze hook (mode B) sees the view
expansion, so it can enforce on the base relation regardless of view owner. It must not be
assumed to protect views whose owner bypasses RLS if enforcement is only via `pg_policy`.

---

## 6. `nested-rls-chain` — a policy reads a table that has its own RLS

**Category:** perf · **Rank:** 6

**Evidence**
- Supabase troubleshooting, hierarchical RLS: "JOINs in RLS policies can lead to performance
  degradation":
  <https://supabase.com/docs/guides/troubleshooting/supabase-storage-inefficient-folder-operations-and-hierarchical-rls-challenges-b05a4d>
  (via search).
- Supabase community posts recommend a SECURITY DEFINER helper to skip the inner RLS (also the
  fix for recursion, see `policy-recursion-42p17`): <https://github.com/orgs/supabase/discussions/47525>.
- Design context: why-rls-is-slow.md section 2 (the `comments` nested policy is the slowest query in
  every column).

**Symptom:** child tables (comments, attachments, line items) are an order of magnitude slower
than their parents; EXPLAIN shows SubPlan inside SubPlan, each with its own `Filter`.

**Root cause:** the policy subquery on `comments` is expanded with the **inner** table's policies
too (`documents` policy, then `memberships` policy). Each layer is a SubPlan; no layer is pulled
up, and the inner security quals prevent index-only access.

**Naive policy**
```sql
CREATE POLICY c ON bench.comments FOR SELECT TO rls_app
  USING (EXISTS (SELECT 1 FROM bench.documents d WHERE d.id = comments.doc_id));
```
(with `documents` and `memberships` policies as in issue 3)

**Best-practice fix:** denormalise the tenant key onto the child (`comments.org_id`) so the
policy is flat (see issue 16), or `doc_id = ANY(ARRAY(SELECT ...))` with a SECURITY DEFINER
helper that skips inner RLS.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
SELECT bench.reset();
ALTER TABLE bench.memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE bench.documents   ENABLE ROW LEVEL SECURITY;
ALTER TABLE bench.comments    ENABLE ROW LEVEL SECURITY;
CREATE POLICY m ON bench.memberships FOR SELECT TO rls_app USING (user_id = (SELECT bench.uid()));
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app
  USING (EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = documents.org_id AND m.user_id = (SELECT bench.uid())));
CREATE POLICY c ON bench.comments FOR SELECT TO rls_app
  USING (EXISTS (SELECT 1 FROM bench.documents d WHERE d.id = comments.doc_id));
SET ROLE rls_app; SET app.uid = 1;
\timing on
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.comments;
-- fix variant: flatten at every level
RESET ROLE; SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY; ALTER TABLE bench.comments ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
CREATE POLICY c ON bench.comments FOR SELECT TO rls_app
  USING (doc_id = ANY (ARRAY(SELECT d.id FROM bench.documents d WHERE d.org_id = ANY (ARRAY(SELECT bench.my_orgs())))));
SET ROLE rls_app; SET app.uid = 1;
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.comments;
```

**Measure:** time, number of nested SubPlans, rows removed at each level, depth sweep (add a
third level to see the multiplicative effect).

**Measured (scale 1):** nested SubPlans 22 ms; flattened arrays 1.3 ms.

**pg_auth:** **Y.** `authz.effective` makes the child policy one indexed lookup regardless of
chain depth; mode B joins it directly.

---

## 7. `non-leakproof-operators-lose-index` — LIKE / ILIKE / jsonb / lower() under RLS

**Category:** perf · **Rank:** 7

**Evidence**
- Tom Lane and Stephen Frost on pgsql-general, "GIST/GIN index not used with Row Level Security":
  <https://www.postgresql.org/message-id/14241.1565725716%40sss.pgh.pa.us> (via search).
- pgsql-hackers "Row Level Security - leakproof-ness and performance implications"
  (258 non-leakproof operator functions counted at the time):
  <https://postgrespro.com/list/thread-id/2427614> (via search).
- Stack Overflow 48230535 "PostgreSQL query not using index when RLS is enabled" (cited by
  sigmoid.at; not opened): <https://sigmoid.at/post/2021/09/23/postgresql_row_level_security/>.
- CYBERTEC "When is a function leakproof?":
  <https://www.cybertec-postgresql.com/en/when-is-a-function-leakproof/> (via search).
- pgsql-general "when is RLS policy applied" (seq scan with the non-leakproof qual next to the
  policy): <https://www.postgresql.org/message-id/6f3e80dd-b84b-a381-7e0a-86492528d644%40aklaver.com>.

**Symptom:** a `LIKE '%x%'` search with a pg_trgm index, a `@>` on a jsonb GIN index, or a
`lower(email) = $1` expression index uses the policy index instead, then filters 100k rows. Plain
`=` on the same table uses the right index. Statistics for the user's qual may also be ignored
(`examine_variable` refuses stats for non-leakproof operators on RLS tables, src: `selfuncs.c`,
unverified line), so estimates are default guesses.

**Root cause:** security-barrier ordering. A user qual that is not leakproof is evaluated only
after the policy quals, and cannot become an index condition (`restriction_is_securely_promotable`
and `contain_leaked_vars`, src: `restrictinfo.c` / `indxpath.c`, unverified line). `~~`, `~~*`,
jsonb operators, `@@`, `lower()` and enum/numeric comparisons are not leakproof. The cost is
proportional to what the **policy** lets through, so broad subjects (power users, admins) are
hit hardest; small tenants hide the problem.

**Naive policy:** any policy; the problem is in the user query shape.

**Best-practice fix:** none that is fully safe in plain RLS. Options people use: marking
functions LEAKPROOF (superuser, **unsafe**, see issue 20), generated columns with `=`
comparisons, `text_pattern_ops`, SECURITY DEFINER search functions, putting the user predicate in
the policy-owning layer. pg_auth's design: mode B flattens the policy into a join, which is only
sound when no non-leakproof user qual touches the protected table, so those queries fall back to
mode A.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX IF NOT EXISTS documents_body_trgm ON bench.documents USING gin (body gin_trgm_ops);
CREATE INDEX IF NOT EXISTS documents_meta_gin  ON bench.documents USING gin (meta jsonb_path_ops);
CREATE INDEX IF NOT EXISTS documents_title_lower ON bench.documents (lower(title) text_pattern_ops);
ANALYZE bench.documents;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
\timing on
-- baseline (owner, no RLS): trigram / GIN index used, ~0.1 ms
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.documents WHERE body LIKE '%abc123%';
SET ROLE rls_app; SET app.uid = 2;            -- power user: policy lets 100k rows through
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.documents WHERE body LIKE '%abc123%';
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.documents WHERE meta @> '{"status":"draft","n":7}';
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.documents WHERE lower(title) = 'doc 77';
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.documents WHERE body = 'abc';   -- control: leakproof, index used
-- check: SELECT oprname, proleakproof FROM pg_operator o JOIN pg_proc p ON p.oid = o.oprcode WHERE oprname IN ('~~','~~*','@>','=');
```

**Measure:** time, index chosen, `Rows Removed by Filter`, estimate error, per operator class.
Sweep policy selectivity (small vs power subject).

**Measured (scale 1, power user):** `LIKE` 10.5 ms (100,000 rows filtered) vs 0.12 ms without RLS;
`@>` 8.4 ms vs 0.1 ms; `=` control uses the trigram index (0.17 ms). For the small user
the policy index is selective enough that the loss is invisible (0.3 ms).

**pg_auth:** **P.** Cannot make non-leakproof operators safe, but can keep indexable user quals
by joining from the small side (mode B) when the planner may; falls back to mode A otherwise.
The benchmark must include both branches of that decision.

---

## 8. `order-by-limit-selective-policy` — planner walks the wrong index

**Category:** perf · **Rank:** 8

**Evidence**
- CockroachDB issue (RLS blocks LIMIT pushdown; it notes that Postgres pushes leakproof quals and
  limit through its barrier): <https://github.com/cockroachdb/cockroach/issues/176115>
  (via search; the Postgres claim is theirs, UNVERIFIED).
- pgsql-performance "Why is query performance on RLS enabled Postgres worse?" (14 rows returned
  after 4.2M removed): URL under issue 3.
- General ORDER BY + LIMIT planner behaviour (not RLS specific):
  <https://www.postgresql.org/message-id/6B8FADC8-9692-439F-8AA4-0CAA287FCEB0%40me.com>.
- **No dedicated RLS + ORDER BY LIMIT thread found** beyond the above, so the RLS-specific
  evidence is the why-rls-is-slow.md measurements and the repro here. Mark as UNVERIFIED externally.

**Symptom:** "latest 20 documents" is instant for a broad user and slow (or the reverse) for a
narrow one, with `Rows Removed by Filter` close to the table size.

**Root cause:** the policy is a filter whose selectivity is unknown to the planner (a hashed
SubPlan or InitPlan array has default estimates: here rows=100000, i.e. 50%). With `LIMIT 20` the
planner assumes it can stop after ~40 rows of an `ORDER BY created_at` index walk and picks that
walk. A subject whose visible rows are old forces the walk through almost the whole index. The
reverse choice hurts broad subjects when the planner picks "filter then sort" instead.

**Naive policy**
```sql
USING (EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = documents.org_id AND m.user_id = (SELECT bench.uid())))
```

**Best-practice fix:** none that is stable for both subjects. App-layer remedies (extra
`WHERE org_id = $1` from the client) are out of scope. Partial fix: `ANY(ARRAY())` gives an
indexable condition so the small subject drives from `org_id`.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app
  USING (EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = documents.org_id AND m.user_id = (SELECT bench.uid())));
SET ROLE rls_app;
SET app.uid = 1;   -- small, visible rows are the OLDEST
EXPLAIN (ANALYZE, COSTS ON, TIMING OFF) SELECT * FROM bench.documents ORDER BY created_at DESC LIMIT 20;
SET app.uid = 2;   -- power, visible rows are the NEWEST
EXPLAIN (ANALYZE, COSTS ON, TIMING OFF) SELECT * FROM bench.documents ORDER BY created_at DESC LIMIT 20;
-- compare with  USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())))
```

**Measure:** execution time, estimated vs actual rows at the filter (est. 100000), `Rows Removed
by Filter`, which index the scan used, behaviour at both subject sizes.

**Measured (scale 1):** small user 13 ms (199,800 rows removed by filter on the `created_at`
index, estimate 100,000 vs actual 20-200); power user 0.11 ms. The array form picks the `org_id`
index for the small user (0.08 ms) and the sort index for the power user.

**pg_auth:** **Y.** Needs mode B for the small subject (drive from permissions) and the
cardinality hook (now: estimates from the subject's ids, design.md) so the planner can choose per subject.

---

## 9. `update-select-coupling` — UPDATE needs a SELECT policy; new row must pass it

**Category:** correctness · **Rank:** 9

**Evidence**
- Supabase discussion #6765 ("new row violates row-level security policy"):
  <https://github.com/orgs/supabase/discussions/6765> (via search). Explainers:
  <https://dev.to/joancomadran/supabase-new-row-violates-row-level-security-policy-every-cause-and-the-fix-136j>.
- pgsql-hackers bug report "RLS policy FOR SELECT is used to check new rows" (Laurenz Albe, 2023):
  <https://postgresql.org/message-id/aee893f1ec3ca8f62a0da2fc2f9f8b73920f9f9d.camel%40cybertec.at>
  (via search; soft-delete example).
- BUG #19359 "Upserts require insert policies in the update path" (PG 18.1):
  <https://www.postgresql.org/message-id/19359-fb1d273ab01b8b22%40postgresql.org> (via search).

**Symptom:** (a) `UPDATE` affects 0 rows with no error because there is no SELECT policy;
(b) `INSERT ... RETURNING` fails although an INSERT policy exists; (c) a soft-delete `UPDATE ...
SET deleted = true` is rejected with "new row violates row-level security policy" because the
SELECT policy hides deleted rows; (d) upsert needs INSERT, UPDATE and SELECT policies.

**Root cause:** when a statement needs to read the target row (WHERE on columns, RETURNING,
ON CONFLICT DO UPDATE), SELECT policies are applied to the existing row, and also to the **new**
row as an extra WITH CHECK (src: `get_row_security_policies` in `rowsecurity.c`, unverified line).
Absent SELECT policy = default deny for the read.

**Naive policy:** `FOR UPDATE ... USING (...) WITH CHECK (...)` with no `FOR SELECT` policy; or a
SELECT policy of `NOT deleted AND ...`.

**Best-practice fix:** write the SELECT policy to cover whatever UPDATE/RETURNING must read; to
allow soft delete, move the filter out of the SELECT policy (views / app queries) or use
SECURITY DEFINER delete functions. Test every verb x policy combination.

**Repro**
```sql
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE bench.documents ADD COLUMN IF NOT EXISTS deleted boolean NOT NULL DEFAULT false;
CREATE POLICY u ON bench.documents FOR UPDATE TO rls_app
  USING (org_id = ANY (ARRAY(SELECT bench.my_orgs()))) WITH CHECK (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
SET ROLE rls_app; SET app.uid = 1;
UPDATE bench.documents SET title = 'x' WHERE id = 200000;          -- (a) UPDATE 0, no error
RESET ROLE;
CREATE POLICY i ON bench.documents FOR INSERT TO rls_app WITH CHECK (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
SET ROLE rls_app; SET app.uid = 1;
BEGIN; INSERT INTO bench.documents (org_id, owner_id, title, body, meta, created_at)
  VALUES (2000, 1, 't', 'b', '{}', now()) RETURNING id; ROLLBACK;   -- (b) ERROR
RESET ROLE;
CREATE POLICY s ON bench.documents FOR SELECT TO rls_app
  USING (NOT deleted AND org_id = ANY (ARRAY(SELECT bench.my_orgs())));
SET ROLE rls_app; SET app.uid = 1;
BEGIN; UPDATE bench.documents SET deleted = true WHERE id = 200000; ROLLBACK;   -- (c) ERROR
```

**Measure:** a correctness matrix (verb x policy set x RETURNING / ON CONFLICT) with expected vs
actual rows/errors. No latency.

**Measured:** (a) 0 rows silently; (b) and (c) ERROR 42501 "new row violates row-level security
policy for table documents".

**pg_auth:** **P.** The engine must define its verb semantics explicitly (read, write, delete,
write-requires-read) and generate matching SELECT/UPDATE/INSERT policies. Include this matrix
as conformance tests.

---

## 10. `pool-session-state-leak` — identity in a GUC across pooled transactions

**Category:** correctness (also causes outages) · **Rank:** 10

**Evidence**
- Drizzle/Prisma measurements ("set_config(..., false)" leaked rows between requests;
  `set_config(..., true)` outside a transaction returned 0 rows; only `db.transaction` works):
  <https://now-next.nl/en/insights/drizzle-postgresql-row-level-security-tenant/> (via search).
- <https://dev.to/qays_kadhim_c3fea1c94957f/the-set-local-advice-is-right-and-it-understates-the-problem-1f62>
  and <https://theroadtoenterprise.com/blog/postgres-rls-multi-tenant-saas> (via search).
- PgBouncer transaction mode and RLS: <https://daily.dev/posts/pgbouncer-and-rls-tenant-context-set-local-shared-plans-wvpn1fj2i>
  (via search; claims about 267k leaked rows and a 10x plan-sharing slowdown are UNVERIFIED).

**Symptom:** one tenant sees another's data under load; or after a transaction ends every query
fails with `invalid input syntax for type integer: ""`.

**Root cause:** session-level `SET` / `set_config(.., false)` survives on a pooled server
connection. `SET LOCAL` outside a transaction is a no-op with a warning. After a transaction
that used `SET LOCAL`, the custom GUC reads as an empty string (not NULL), so
`current_setting('x')::int` raises. Unknown custom GUC: `current_setting('x')` raises unless
`missing_ok` is true.

**Naive policy**
```sql
USING (org_id = current_setting('app.org_id')::int)
```

**Best-practice fix:** set identity with `set_config(k, v, true)` as the first statement of an
explicit transaction; policies read `nullif(current_setting(k, true), '')::int` and fail closed
(NULL matches nothing). Grant the role no ability to `SET` the GUC itself (use a role check /
reserved prefix) and wrap in one function.

**Repro**
```sql
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (org_id = current_setting('app.org_id')::int);
SET ROLE rls_app;
BEGIN; SET LOCAL app.org_id = 5; SELECT count(*) FROM bench.documents; COMMIT;   -- 100 rows
SELECT current_setting('app.org_id');                          -- '' (not NULL)
SELECT count(*) FROM bench.documents;                          -- ERROR: invalid input syntax for type integer: ""
SET app.org_id = 5;                                            -- session-level: stays for the next client
RESET ROLE; SET ROLE rls_app;
SELECT count(*) FROM bench.documents;                          -- still org 5's rows
SET app.org_id = 7;
SET LOCAL app.org_id = 9;                                      -- WARNING: no effect outside a transaction
SHOW app.org_id;                                               -- 7
-- note: a role that can run SET app.org_id can impersonate any tenant. That is the point of issue 12's reserved-GUC rule.
```

**Measure:** correctness: leaked rows across N concurrent clients on a pooled connection
(pgbench with a pool simulator or PgBouncer in transaction mode), count of `""` errors.

**Measured:** transaction-scoped value works (100 rows); after COMMIT the GUC is `''` and the next
query errors; session-level SET persists; SET LOCAL in autocommit warns and has no effect.

**pg_auth:** **P.** The extension should own the identity channel (a function setting a
protected context, fail-closed when unset) so that applications cannot get it wrong; it cannot
fix pooler misconfiguration.

---

## 11. `policy-recursion-42p17` — policy reads its own table

**Category:** correctness (availability) · **Rank:** 11

**Evidence**
- Supabase discussion "RLS policy causes infinite recursion" (#47525):
  <https://github.com/orgs/supabase/discussions/47525> (via search).
- Supabase troubleshooting "Infinite recursion detected in policy for relation":
  <https://supabase.com/kb/troubleshooting/storage-error-database-schema-is-incompatible-when-uploading-files>
  (via search); Supabase RLS docs recommend a SECURITY DEFINER helper (owner needs BYPASSRLS).
- <https://www.guardlayer.io/blog/supabase-rls-infinite-recursion> (via search).

**Symptom:** `ERROR: infinite recursion detected in policy for relation "memberships"`
(SQLSTATE 42P17) on any query touching the table.

**Root cause:** expanding the policy on `memberships` requires reading `memberships`, which
re-expands the same policy (`fireRIRrules` cycle detection in `rewriteHandler.c`, unverified
line). Cross-table cycles (A reads B, B reads A) trigger the same error.

**Naive policy**
```sql
CREATE POLICY m ON bench.memberships FOR SELECT TO rls_app
  USING (org_id IN (SELECT org_id FROM bench.memberships WHERE user_id = bench.uid()));
```

**Best-practice fix:** SECURITY DEFINER helper with `SET search_path = ''` and an owner that
bypasses RLS (this introduces issue 12 if the helper takes a user id), or a simple non-recursive
`user_id = uid` policy on the membership table and separate helpers for "peers".

**Repro**
```sql
SELECT bench.reset();
ALTER TABLE bench.memberships ENABLE ROW LEVEL SECURITY;
CREATE POLICY m ON bench.memberships FOR SELECT TO rls_app
  USING (org_id IN (SELECT org_id FROM bench.memberships WHERE user_id = bench.uid()));
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.memberships;       -- ERROR 42P17
```

**Measure:** error vs success; for the fix, latency of the helper path (issue 1).

**Measured:** ERROR: infinite recursion detected in policy for relation "memberships".

**pg_auth:** **Y.** Policies are generated from the compiled model over `authz.effective`,
which is not itself RLS-protected for the enforcing path, so no cycle can exist by construction.

---

## 12. `secdef-helper-oracle` — SECURITY DEFINER helper that takes a user id

**Category:** correctness · **Rank:** 12

**Evidence**
- Supabase RLS docs: security definer functions bypass RLS "without any penalties", must set
  `search_path = ''`, and must not live in an exposed schema:
  <https://supabase.com/docs/guides/database/postgres/row-level-security> (opened).
- Same advice in the recursion threads (issue 11).
- **UNVERIFIED:** I did not find a public incident write-up of the membership-enumeration
  variant. The repro demonstrates the mechanism directly.

**Symptom:** any API user can call `is_member_of(<other user>, <org>)` through PostgREST RPC
(functions are `EXECUTE` to PUBLIC by default) and enumerate other users' memberships even
though the membership table itself is protected.

**Root cause:** SECURITY DEFINER runs as the owner, skipping RLS; the helper trusts its
`p_user` argument instead of deriving the subject from the session. Default `EXECUTE` to PUBLIC.
Also: without `SET search_path`, object-name hijacking.

**Naive policy / helper**
```sql
CREATE FUNCTION bench.is_member_of(p_user int, p_org int) ... SECURITY DEFINER ...
```

**Best-practice fix:** helper derives the subject itself (no user-id parameter) or takes only
row keys; `REVOKE EXECUTE ... FROM PUBLIC`; fixed `search_path`; keep helpers in a non-exposed
schema.

**Repro**
```sql
SELECT bench.reset();
ALTER TABLE bench.memberships ENABLE ROW LEVEL SECURITY;
CREATE POLICY m ON bench.memberships FOR SELECT TO rls_app USING (user_id = bench.uid());
CREATE OR REPLACE FUNCTION bench.is_member_of(p_user int, p_org int) RETURNS boolean
  LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
  AS $$ SELECT EXISTS (SELECT 1 FROM bench.memberships WHERE user_id = p_user AND org_id = p_org) $$;
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) AS visible_memberships FROM bench.memberships;        -- 2
SELECT u AS other_user, count(*) FILTER (WHERE bench.is_member_of(u, o)) AS orgs_found
FROM generate_series(3, 8) u, generate_series(1, 2000) o GROUP BY u ORDER BY u;   -- 2 orgs each
```

**Measure:** correctness: rows of other subjects revealed; assert 0.

**Measured:** user 1 sees 2 membership rows directly, yet learns each other user's 2 orgs through
the helper.

**pg_auth:** **Y.** The extension's check functions take no subject argument and read the subject
from the protected context; add a test that no `authz` function is executable by PUBLIC.

---

## 13. `rls-bypass-paths` — owner, BYPASSRLS, materialized views, pg_dump, replication

**Category:** correctness · **Rank:** 13

**Evidence**
- Bytebase "Postgres Row-Level Security Footguns" (owner bypass, FORCE, matviews, COPY exports):
  <https://www.bytebase.com/blog/postgres-row-level-security-footguns/> (via search).
- PostgreSQL docs, "Logical replication restrictions" (tables only; matviews unsupported):
  <https://www.postgresql.org/docs/current/logical-replication-restrictions.html> (via search).
- pg_dump on Supabase "query would be affected by row-level security policy":
  <https://dev.to/aktisdev/pgdump-on-supabase-query-would-be-affected-by-row-level-security-policy-51a3>
  (via search).
- pgDash, "Exploring Row Level Security in PostgreSQL" (owner, FORCE):
  <https://pgdash.io/blog/exploring-row-level-security-in-postgres.html> (via search).
- "A replication user needs BYPASSRLS or consumers receive an empty stream" appears only in
  <https://queryplane.com/blog/postgres-row-level-security-in-practice/>: **UNVERIFIED** (secondary
  source, not in PG docs).

**Symptom:** the app connects as the table owner (migration user) and RLS "does nothing"; a
nightly materialized view contains all tenants; a backup tool errors or produces empty tables;
`COPY ... FROM` is rejected.

**Root cause:** superusers and BYPASSRLS roles always skip RLS; owners skip it unless
`FORCE ROW LEVEL SECURITY`; RLS is not supported on materialized views (they hold whatever the
refresher could read); `pg_dump` defaults to `row_security = off` and errors unless allowed;
`COPY FROM` is not supported on RLS tables; logical replication applies to tables, not
policies.

**Naive:** application role == owner; `CREATE MATERIALIZED VIEW` over a protected table.

**Best-practice fix:** separate owner and application roles; `FORCE ROW LEVEL SECURITY`; matviews
only for tenant-agnostic aggregates (or per-tenant tables); a dedicated BYPASSRLS backup role.

**Repro**
```sql
SELECT bench.reset();
DO $$ BEGIN CREATE ROLE rls_owner NOLOGIN; EXCEPTION WHEN duplicate_object THEN NULL; END $$;
DO $$ BEGIN CREATE ROLE rls_bypass NOLOGIN BYPASSRLS; EXCEPTION WHEN duplicate_object THEN NULL; END $$;
GRANT USAGE ON SCHEMA bench TO rls_owner, rls_bypass; GRANT SELECT ON bench.documents TO rls_bypass;
ALTER TABLE bench.documents OWNER TO rls_owner;
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app, rls_owner USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
SET app.uid = 1;
SET ROLE rls_app;    SELECT 'rls_app' AS role, count(*) FROM bench.documents; RESET ROLE;             -- 200
SET ROLE rls_owner;  SELECT 'owner, no FORCE', count(*) FROM bench.documents; RESET ROLE;            -- 200000
SET ROLE rls_bypass; SELECT 'BYPASSRLS', count(*) FROM bench.documents; RESET ROLE;                  -- 200000
ALTER TABLE bench.documents FORCE ROW LEVEL SECURITY;
SET ROLE rls_owner;  SELECT 'owner, FORCE', count(*) FROM bench.documents; RESET ROLE;               -- 200
ALTER TABLE bench.documents NO FORCE ROW LEVEL SECURITY; ALTER TABLE bench.documents OWNER TO CURRENT_USER;
-- matview built by the owner contains every tenant:
CREATE MATERIALIZED VIEW bench.doc_counts AS SELECT org_id, count(*) AS n FROM bench.documents GROUP BY org_id;
GRANT SELECT ON bench.doc_counts TO rls_app;
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.doc_counts;                        -- 2000 orgs, not 2
COPY bench.documents FROM STDIN;                              -- ERROR: COPY FROM not supported with row-level security
```

**Measure:** a bypass matrix: role kind x access path (table, view, matview, COPY TO, pg_dump,
logical slot) with expected row counts.

**Measured:** 200 / 200000 / 200000 / 200 (owner with FORCE); matview 2000 rows visible to the app
role; `COPY FROM` refused. `COPY TO` was not shown here because file COPY needs
server-file privileges; `COPY ... TO STDOUT` applies RLS (not run).

**pg_auth:** **P.** The extension can ship a catalog audit (tables with RLS but owner-used
roles, matviews over protected tables, roles with BYPASSRLS that are login roles). It cannot
change the semantics of these paths.

---

## 14. `partition-direct-access` — leaf partitions skip the parent's policy

**Category:** correctness · **Rank:** 14

**Evidence**
- Search results summarising the behaviour: policies belong to the table named in the query;
  "Implementing RLS on billion-row hash-partitioned tables" enables RLS and policies on each of 74
  partitions: <https://medium.com/@rajarsi3997/implementing-row-level-security-on-billion-row-hash-partitioned-postgresql-tables-f617ce5045ef>
  (via search; I did not open it).
- pgDash (inheritance: policy on the child only applies when the child is queried; parent query
  can return child rows): <https://pgdash.io/blog/exploring-row-level-security-in-postgres.html>.
- EDB on row movement (policy of the table named in the UPDATE applies):
  <https://www.enterprisedb.com/blog/row-movement-across-postgresql-partitions-made-easy>
  (via search).
- PostgreSQL docs, CREATE POLICY ("policies of the table named in the query"):
  <https://www.postgresql.org/docs/current/sql-createpolicy.html>.

**Symptom:** `SELECT * FROM documents_p_2` returns rows the parent query would hide.

**Root cause:** the policy set is looked up for the relation in the RangeTblEntry the user wrote;
partition pruning/expansion happens afterwards and does not re-apply the parent's policies.
Blanket `GRANT ... ON ALL TABLES IN SCHEMA` also grants the leaves. With RLS enabled on a leaf
but no policy, the leaf is default-deny (0 rows).

**Naive:** policy on the partitioned parent only.

**Best-practice fix:** grant only on the parent; or enable RLS and create the same policies on
every leaf, including future partitions (event trigger / partition-creation hook).

**Repro** (requires the `documents_p` table from `partitioned-rls-no-pruning`)
```sql
SELECT bench.reset();
ALTER TABLE bench.documents_p ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents_p FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
GRANT SELECT ON bench.documents_p, bench.documents_p_2 TO rls_app;   -- e.g. a blanket GRANT on all tables
SET ROLE rls_app; SET app.uid = 1;
SELECT 'via parent' AS path, count(*) FROM bench.documents_p
UNION ALL SELECT 'partition 2 directly', count(*) FROM bench.documents_p_2;
RESET ROLE;
ALTER TABLE bench.documents_p_2 ENABLE ROW LEVEL SECURITY;
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.documents_p_2;                      -- RLS on, no policy: 0
```

**Measure:** row counts through parent vs each leaf; a catalog check that every leaf of an
RLS parent has `relrowsecurity` and a policy.

**Measured:** via parent 200; leaf 2 directly 14300; leaf with RLS enabled but no policy 0.

**pg_auth:** **Y.** Install and maintain policies on every leaf (as pg_auth does: design.md), plus a
test that creates a new partition and asserts it is protected.

---

## 15. `partitioned-rls-no-pruning` — the policy array does not prune partitions

**Category:** perf · **Rank:** 15

**Evidence**
- Design note: why-rls-is-slow.md section 2 (partitioned count / latest-20 columns).
- Amit Langote, "Postgres: on performance of prepared statements with partitioning" (generic
  plans cannot prune at plan time): <https://amitlan.com/2022/05/16/param-query-partition-woes.html>
  (via search; adjacent, not RLS-specific).
- **UNVERIFIED externally:** I found no public RLS + partition-pruning bug report. Evidence is
  the repro.

**Symptom:** a tenant-partitioned table queried by a small tenant still opens every partition;
latency grows with partition count (planning time and scan startup per partition).

**Root cause:** the policy `org_id = ANY(InitPlan array)` is not a plan-time constant, and for
hash partitioning the array elements are not known to the pruner at plan time; the sublink in
the policy is never pulled up into a join, so run-time pruning from a join cannot occur either
(see issue 3). The EXPLAIN shows all partitions as children of `Append`.

**Naive policy:** `USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())))` on the parent.

**Best-practice fix:** put the tenant literal in the query (app layer, out of scope), partition
on a key present in the permission rows, or use mode B.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
DROP TABLE IF EXISTS bench.documents_p;
CREATE TABLE bench.documents_p (LIKE bench.documents INCLUDING DEFAULTS) PARTITION BY HASH (org_id);
SELECT format('CREATE TABLE bench.documents_p_%s PARTITION OF bench.documents_p FOR VALUES WITH (MODULUS 16, REMAINDER %s)', i, i)
  FROM generate_series(0,15) i \gexec
INSERT INTO bench.documents_p (id, org_id, owner_id, title, body, meta, created_at)
  OVERRIDING SYSTEM VALUE SELECT id, org_id, owner_id, title, body, meta, created_at FROM bench.documents;
ALTER TABLE bench.documents_p ADD PRIMARY KEY (id, org_id);
CREATE INDEX ON bench.documents_p (org_id);
CREATE INDEX ON bench.documents_p (created_at DESC);
VACUUM ANALYZE bench.documents_p;
GRANT SELECT ON bench.documents_p TO rls_app;
SELECT bench.reset();
ALTER TABLE bench.documents_p ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents_p FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
SET ROLE rls_app; SET app.uid = 1;
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.documents_p;                          -- 16 index scans
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT * FROM bench.documents_p ORDER BY created_at DESC LIMIT 20;
RESET ROLE;
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT count(*) FROM bench.documents_p WHERE org_id IN (1999, 2000);  -- literal: 2 partitions
-- sweep: MODULUS 16 -> 64 -> 256
```

**Measure:** partitions scanned (count `Scan` children / `Subplans Removed`), planning time,
execution time, as the partition count grows.

**Measured (scale 1, 16 partitions):** all 16 partitions scanned for the small user,
`ORDER BY created_at DESC LIMIT 20` with `Merge Append` 11 ms; the literal-`IN` form scans only
2 partitions (0.1 ms).

**pg_auth:** **Y.** Include the partition key in `authz.effective` and use mode B (join gives
run-time pruning) or an array that the pruner can use.

---

## 16. `join-two-rls-tables` — joins between two RLS-protected tables

**Category:** perf · **Rank:** 16

**Evidence**
- Supabase troubleshooting/community advice to avoid joins in `USING` and denormalise the tenant:
  <https://github.com/orgs/community/discussions/189420> (via search).
- Design note: why-rls-is-slow.md section 2 ("comments" row) and the table of join shapes.
- **UNVERIFIED externally** for a specific public RLS join-misestimate report; third-party blogs
  repeat the generic advice only.

**Symptom:** `documents JOIN comments` is slow even though each table alone is fast; EXPLAIN
shows a nested loop driven by a seq scan with a `hashed SubPlan` filter on **both** sides and a
row estimate of tens of thousands against hundreds actual.

**Root cause:** each side's policy is an opaque filter with default selectivity (50%); join
order and method are chosen on those guesses (here a nested loop with 200 inner index probes,
each re-filtering with its own SubPlan). Row estimates multiply across the join.

**Naive policy:** `EXISTS (SELECT 1 FROM memberships ...)` on both tables, each with its own
`org_id` column.

**Best-practice fix:** denormalise `org_id` onto both tables and use the same indexable
`= ANY(ARRAY())` on each, so the planner can start from either side.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
ALTER TABLE bench.comments ADD COLUMN IF NOT EXISTS org_id int;
UPDATE bench.comments c SET org_id = d.org_id FROM bench.documents d WHERE d.id = c.doc_id AND c.org_id IS NULL;
CREATE INDEX IF NOT EXISTS comments_org_idx ON bench.comments (org_id);
ANALYZE bench.comments;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY; ALTER TABLE bench.comments ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app
  USING (EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = documents.org_id AND m.user_id = (SELECT bench.uid())));
CREATE POLICY c ON bench.comments FOR SELECT TO rls_app
  USING (EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = comments.org_id AND m.user_id = (SELECT bench.uid())));
SET ROLE rls_app; SET app.uid = 1;
EXPLAIN (ANALYZE, COSTS ON, TIMING OFF) SELECT d.title, count(*) FROM bench.documents d JOIN bench.comments c ON c.doc_id = d.id
  GROUP BY d.title ORDER BY 2 DESC LIMIT 10;
RESET ROLE; SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY; ALTER TABLE bench.comments ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
CREATE POLICY c ON bench.comments  FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
SET ROLE rls_app; SET app.uid = 1;
EXPLAIN (ANALYZE, COSTS ON, TIMING OFF) SELECT d.title, count(*) FROM bench.documents d JOIN bench.comments c ON c.doc_id = d.id
  GROUP BY d.title ORDER BY 2 DESC LIMIT 10;
```

**Measure:** estimated vs actual rows at the join, join method, time; sweep subject size.

**Measured (scale 1):** SubPlan form: Nested Loop estimated 50,000 rows vs 200 actual, 10.3 ms.
Array form: Hash Join estimated 5 vs 200 actual, 0.2 ms.

**pg_auth:** **Y.** Mode B puts both protected tables into one join tree, giving the planner
real statistics on `authz.effective`.

---

## 17. `tenant-skew-bad-estimates` — `tenant_id = (select setting)` hides the value from statistics

**Category:** perf · **Rank:** 17

**Evidence**
- Supabase and PostgREST advice to wrap `current_setting` in `(select ...)` (issues 1-2) has
  this side effect; the PostgREST thread does not mention estimates.
- Generic/custom plan background: <https://www.postgresql.org/docs/current/runtime-config-query.html>
  (`plan_cache_mode`); <https://dev.to/franckpachot/postgresql-query-planner-parameters-and-prepared-statements-3o5a>
  (via search).
- **UNVERIFIED externally:** no public report tying this specifically to RLS. The repro shows it.

**Symptom:** queries are fast for most tenants and pathological for the big tenant (or vice
versa); `EXPLAIN` shows `rows=2` where 2000 are returned, and the planner picks a bitmap AND or a
nested loop sized for a tiny set.

**Root cause:** the InitPlan parameter's value is unknown at plan time, so the selectivity of
`tenant_id = $param` is `1/ndistinct` (here 100 of 200k rows), ignoring the MCV list. A literal
would use the MCV entry (50% for tenant 1). The wrap that fixes per-row evaluation (issues 1-2)
is what causes it. Skew is the norm in multi-tenant data.

**Naive policy:** `USING (tenant_id = (SELECT nullif(current_setting('app.tenant', true), '')::int))`

**Best-practice fix:** none in plain RLS short of per-tenant partial indexes / custom plans.
Compare with putting the literal in the query.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
DROP TABLE IF EXISTS bench.events;
CREATE TABLE bench.events (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, tenant_id int NOT NULL, kind int NOT NULL, ts timestamptz NOT NULL, payload text);
INSERT INTO bench.events (tenant_id, kind, ts, payload)
SELECT CASE WHEN g % 2 = 0 THEN 1 ELSE 2 + g % 1999 END, (g / 2) % 50, now() - (g || ' seconds')::interval, md5(g::text)
FROM generate_series(1, 200000) g;                      -- tenant 1 owns 50% of rows
CREATE INDEX ON bench.events (tenant_id, ts DESC);
CREATE INDEX ON bench.events (kind);
ANALYZE bench.events;
GRANT SELECT ON bench.events TO rls_app;
SELECT bench.reset();
ALTER TABLE bench.events ENABLE ROW LEVEL SECURITY;
CREATE POLICY t ON bench.events FOR SELECT TO rls_app USING (tenant_id = (SELECT nullif(current_setting('app.tenant', true), '')::int));
SET ROLE rls_app; SET app.tenant = 1;
EXPLAIN (ANALYZE, COSTS ON, TIMING OFF) SELECT * FROM bench.events WHERE kind = 7 ORDER BY ts DESC LIMIT 50;
SET app.tenant = 77;
EXPLAIN (ANALYZE, COSTS ON, TIMING OFF) SELECT * FROM bench.events WHERE kind = 7 ORDER BY ts DESC LIMIT 50;
RESET ROLE;
EXPLAIN (ANALYZE, COSTS ON, TIMING OFF) SELECT * FROM bench.events WHERE tenant_id = 1 AND kind = 7 ORDER BY ts DESC LIMIT 50;
```

**Measure:** estimated vs actual rows at the tenant condition; plan shape; latency for the big
tenant vs a small one; also run with `PREPARE` and `plan_cache_mode = force_generic_plan`.

**Measured (scale 1):** policy form estimates 100 rows (actual 100,000) and builds a BitmapAnd;
big tenant 3.1 ms, small tenant 0.1 ms; the literal form chooses a single index scan on
`(tenant_id, ts)` (0.12 ms, estimate 1987). The absolute gap is small at scale 1; run at
`scale=10+` and add a join above it to see the compounding effect.

**pg_auth:** **Y.** Needs the cardinality hook (now: estimates from the subject's ids, design.md): expose the subject's allowed-set
size or inline the subject so ordinary statistics on `authz.effective` apply.

---

## 18. `hierarchical-permissions` — folders, org trees, inherited grants in a policy

**Category:** perf · **Rank:** 18

**Evidence**
- Supabase troubleshooting "Storage: inefficient folder operations and hierarchical RLS
  challenges" (recommends a folder table + joins, warns about join cost):
  <https://supabase.com/docs/guides/troubleshooting/supabase-storage-inefficient-folder-operations-and-hierarchical-rls-challenges-b05a4d>
  (via search).
- OpenFGA / Zanzibar-style design (closure / set-of-sets) is covered in
  [`scaling-db-native.md`](scaling-db-native.md); not repeated here.
- **UNVERIFIED externally:** no public "WITH RECURSIVE in a policy is slow" report found; the
  search returned only the Supabase storage article.

**Symptom:** access to a leaf resource requires walking ancestors; listing the files in a
folder is O(files x depth) policy work.

**Root cause:** a correlated recursive CTE (or recursive SQL function) runs per row inside the
policy's EXISTS SubPlan; there is no pull-up, no caching between rows, and the recursion is
re-evaluated for each file even when all files share a folder.

**Naive policy**
```sql
USING (EXISTS (
  WITH RECURSIVE anc(id) AS (
    SELECT files.folder_id
    UNION ALL SELECT f.parent_id FROM bench.folders f JOIN anc ON f.id = anc.id WHERE f.parent_id IS NOT NULL)
  SELECT 1 FROM anc JOIN bench.folder_grants g ON g.folder_id = anc.id AND g.user_id = (SELECT bench.uid())))
```

**Best-practice fix:** precompute a closure table `(ancestor, descendant)` (or ltree/materialised
path) and use `folder_id = ANY(ARRAY(SELECT descendant FROM closure JOIN grants ...))`. Cost
moves to writes (moving a folder rewrites its subtree's closure rows).

**Repro**
```sql
\if :{?scale} \else \set scale 1 \endif
SET max_parallel_workers_per_gather = 0;
SELECT (50000 * :scale)::int AS nf \gset
DROP TABLE IF EXISTS bench.files, bench.folder_closure, bench.folder_grants, bench.folders CASCADE;
CREATE TABLE bench.folders (id int PRIMARY KEY, parent_id int REFERENCES bench.folders);
CREATE TABLE bench.files (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, folder_id int NOT NULL, name text NOT NULL);
CREATE TABLE bench.folder_grants (user_id int NOT NULL, folder_id int NOT NULL, PRIMARY KEY (user_id, folder_id));
INSERT INTO bench.folders SELECT g, CASE WHEN g = 1 THEN NULL ELSE (g - 2) / 5 + 1 END FROM generate_series(1, :nf) g;  -- 5-ary tree
INSERT INTO bench.files (folder_id, name) SELECT 1 + g % :nf, 'f' || g FROM generate_series(1, 2 * :nf) g;
CREATE INDEX ON bench.files (folder_id);
INSERT INTO bench.folder_grants VALUES (1, 100), (1, 101), (2, 2);
CREATE TABLE bench.folder_closure (ancestor int NOT NULL, descendant int NOT NULL, PRIMARY KEY (ancestor, descendant));
WITH RECURSIVE t(anc, d) AS (
  SELECT id, id FROM bench.folders
  UNION ALL SELECT t.anc, f.id FROM t JOIN bench.folders f ON f.parent_id = t.d)
INSERT INTO bench.folder_closure SELECT anc, d FROM t;
CREATE INDEX ON bench.folder_closure (descendant);
ANALYZE bench.folders; ANALYZE bench.files; ANALYZE bench.folder_grants; ANALYZE bench.folder_closure;
GRANT SELECT ON bench.folders, bench.files, bench.folder_grants, bench.folder_closure TO rls_app;
CREATE OR REPLACE FUNCTION bench.my_folders() RETURNS SETOF int LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
  AS $$ SELECT c.descendant FROM bench.folder_grants g JOIN bench.folder_closure c ON c.ancestor = g.folder_id WHERE g.user_id = bench.uid() $$;
SELECT bench.reset();
ALTER TABLE bench.files ENABLE ROW LEVEL SECURITY;
CREATE POLICY naive ON bench.files FOR SELECT TO rls_app USING (EXISTS (
  WITH RECURSIVE anc(id) AS (
    SELECT files.folder_id
    UNION ALL SELECT f.parent_id FROM bench.folders f JOIN anc ON f.id = anc.id WHERE f.parent_id IS NOT NULL)
  SELECT 1 FROM anc JOIN bench.folder_grants g ON g.folder_id = anc.id AND g.user_id = (SELECT bench.uid())));
\timing on
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.files;                                  -- 624
RESET ROLE; DROP POLICY naive ON bench.files;
CREATE POLICY closure ON bench.files FOR SELECT TO rls_app USING (folder_id = ANY (ARRAY(SELECT bench.my_folders())));
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.files;                                  -- 624
```

**Measure:** query time vs tree depth/fan-out and files per folder; closure maintenance cost when
moving a subtree or adding a grant (writes, not reads); closure table size.

**Measured (scale 1, 50k folders, 100k files, depth ~7):** recursive policy 333 ms; closure +
array 0.5 ms; both return 624 files.

**pg_auth:** **Y.** The closure/set-of-sets in `authz.effective` was the earlier design;
the benchmark must also cover maintenance under churn (the earlier design's plan).

---

## 19. `large-membership-array-overhead` — `= ANY(array)` rebuilds a huge array per statement

**Category:** perf · **Rank:** 19

**Evidence**
- Supabase advice `team_id = ANY(ARRAY(select user_teams()))` "is a big improvement but can still
  take seconds" (from the performance-guide summary, via search; UNVERIFIED in detail).
- Design note: why-rls-is-slow.md section 2: `= ANY(array(...))` is indexable and evaluated once, and
  section 3.1 item 4 (roaring bitmaps for very large sets).
- **UNVERIFIED externally:** no public report of per-statement array-build cost for PK lookups.

**Symptom:** even a primary-key lookup is slow for users with many grants; cost scales with the
size of the subject's allowed set, not with the rows touched.

**Root cause:** the InitPlan runs the set-returning helper to completion, builds an array of N
elements (and for an index scan on the array, sorts/dedups it, `ScalarArrayOpExpr` over btree),
once per statement, before returning the first row. O(N) fixed overhead per query. Prepared
statements re-run the InitPlan each execution.

**Naive policy:** `USING (id = ANY (ARRAY(SELECT bench.my_doc_ids())))` for ACL-style
per-document sharing.

**Best-practice fix:** keep the set small via groups (grant to a group, user to group), or join
instead of materialising (mode B), or compress (bitmap).

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
DROP TABLE IF EXISTS bench.doc_grants;
CREATE TABLE bench.doc_grants (user_id int NOT NULL, doc_id bigint NOT NULL, PRIMARY KEY (user_id, doc_id));
INSERT INTO bench.doc_grants SELECT 1, g FROM generate_series(1, 10) g;
INSERT INTO bench.doc_grants SELECT 9999, id FROM bench.documents WHERE id % 2 = 0;     -- 100k grants
CREATE INDEX ON bench.doc_grants (doc_id);
ANALYZE bench.doc_grants;
GRANT SELECT ON bench.doc_grants TO rls_app;
CREATE OR REPLACE FUNCTION bench.my_doc_ids() RETURNS SETOF bigint LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
  AS $$ SELECT doc_id FROM bench.doc_grants WHERE user_id = bench.uid() $$;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (id = ANY (ARRAY(SELECT bench.my_doc_ids())));
SET ROLE rls_app;
SET app.uid = 1;    EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT * FROM bench.documents WHERE id = 4;
SET app.uid = 9999; EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) SELECT * FROM bench.documents WHERE id = 4;
```

**Measure:** latency of a PK lookup as the allowed-set size sweeps 10 .. 1M; memory
(`pg_backend_memory_contexts`); also compare `IN (SELECT ...)` (hashed) and a join.

**Measured (scale 1):** PK lookup 0.2 ms for 10 grants vs 16 ms for 100k grants.

**pg_auth:** **Y.** Mode B keeps this as an index probe on `authz.effective`; mode A is exposed
to this cost and should be benchmarked as its worst case.

---

## 20. `leaky-function-and-leakproof-marking` — LEAKPROOF used as a performance hack

**Category:** correctness · **Rank:** 20 (but a hard invariant for mode B)

**Evidence**
- pgsql-hackers "Per-role disabling of LEAKPROOF requirements for row-level security?" (people
  asking to relax the rule for speed):
  <https://www.mail-archive.com/pgsql-hackers@lists.postgresql.org/msg196470.html> (via search).
- pgsql-hackers leakproof thread, <https://postgrespro.com/list/thread-id/2427614>; a 1-bit error
  channel is considered a covert channel there (via search).
- CYBERTEC <https://www.cybertec-postgresql.com/en/when-is-a-function-leakproof/> and the
  PostgreSQL docs, "Rules and privileges"/RLS ("security-barrier ... leakproof"):
  <https://www.postgresql.org/docs/current/ddl-rowsecurity.html>.
- pganalyze "5mins E28" (URL under issue 5).

**Symptom:** the performance fix for issue 7 ("mark it LEAKPROOF") makes a user-defined function
run on rows the user cannot see. Also "side-channel" variants: unique-constraint and FK errors
reveal existence of hidden rows; error messages from casts (`invalid input syntax for type
integer: "secret"`) leak values.

**Root cause:** leakproof functions are trusted to run before policy quals. The flag is not
verified; only superusers can set it. A function with a low `COST` and the flag is pushed to the
front of the qual order.

**Naive:** `ALTER FUNCTION f LEAKPROOF` to unlock an index.

**Best-practice fix:** never set LEAKPROOF on a function that can raise errors depending on its
input or logs it; use built-in leakproof operators only (`pg_proc.proleakproof`).

**Repro**
```sql
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
CREATE OR REPLACE FUNCTION bench.spy(t text) RETURNS boolean LANGUAGE plpgsql COST 0.0000001
  AS $$ BEGIN RAISE NOTICE 'spy saw: %', t; RETURN true; END $$;
GRANT EXECUTE ON FUNCTION bench.spy(text) TO rls_app;
SET ROLE rls_app; SET app.uid = 1; SET client_min_messages = notice;
SELECT count(*) FROM bench.documents WHERE bench.spy(title) AND id % 1000 = 0;   -- notices only for visible docs (200)
RESET ROLE;
ALTER FUNCTION bench.spy(text) LEAKPROOF;                                         -- superuser "fix"
SET ROLE rls_app; SET app.uid = 1;
SELECT count(*) FROM bench.documents WHERE bench.spy(title) AND id = 5;           -- doc 5 is in org 1, invisible to user 1
```

**Measure:** count of hidden-row values observed by the user (assert 0). This is the
adversarial test for mode B (the earlier design's plan): leaky functions in `WHERE`,
`JOIN ON`, `ORDER BY`, target list, expression indexes.

**Measured:** without the flag the function sees only visible documents (200 distinct). With the
flag the second query (`id = 5`, a document in an org the user cannot see) prints `spy saw: doc 5`: the function ran on a hidden row.

**pg_auth:** **Y** (as an invariant). Mode B must only rewrite when every user qual touching the
protected table is leakproof, and the RLS policy must stay installed as the fail-closed
fallback. This scenario is the first adversarial test.

---

## 21. `update-delete-bulk-rls-cost` — USING and WITH CHECK per written row

**Category:** perf · **Rank:** 21

**Evidence**
- Supabase performance advice applies to all verbs: <https://supabase.com/docs/guides/database/postgres/row-level-security>
  (opened). Per-verb numbers are not given.
- **UNVERIFIED externally:** I found no public report specific to bulk `UPDATE`/`DELETE` under
  RLS. The repro shows the policy overhead on writes.

**Symptom:** bulk updates and deletes (cleanup jobs, import merges) take seconds longer under RLS
than as owner; slower still with function-based policies.

**Root cause:** `UPDATE` evaluates the UPDATE policy `USING` on every candidate old row and
`WITH CHECK` on every new row, plus the SELECT policy when the statement reads the row (issue 9).
`DELETE` evaluates `USING` (and SELECT when `RETURNING`/WHERE need it). Policy cost is added
per row on top of heap and index maintenance, so the relative effect is smaller than on reads
but absolute cost grows with function-based policies.

**Naive policy:** function-based `USING` and `WITH CHECK` (issue 1).

**Best-practice fix:** the same array form; for maintenance jobs, run as a privileged role
outside RLS.

**Repro**
```sql
SET max_parallel_workers_per_gather = 0;
\timing on
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY s ON bench.documents FOR SELECT TO rls_app USING (bench.is_member_plpgsql(org_id));
CREATE POLICY u ON bench.documents FOR UPDATE TO rls_app USING (bench.is_member_plpgsql(org_id)) WITH CHECK (bench.is_member_plpgsql(org_id));
SET ROLE rls_app; SET app.uid = 2;
BEGIN; UPDATE bench.documents SET title = title || '!' WHERE org_id <= 1000; ROLLBACK;     -- ~100k rows
RESET ROLE; DROP POLICY s ON bench.documents; DROP POLICY u ON bench.documents;
CREATE POLICY s ON bench.documents FOR SELECT TO rls_app USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
CREATE POLICY u ON bench.documents FOR UPDATE TO rls_app
  USING (org_id = ANY (ARRAY(SELECT bench.my_orgs()))) WITH CHECK (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
SET ROLE rls_app; SET app.uid = 2;
BEGIN; UPDATE bench.documents SET title = title || '!' WHERE org_id <= 1000; ROLLBACK;
-- also: same statement as owner (no RLS) for the floor
```

**Measure:** wall time, WAL volume (`pg_stat_wal`), policy function calls; ratio against the
owner's un-protected statement.

**Measured (scale 1):** 2.9 s (plpgsql policy) vs 2.0 s (array policy) for ~100k rows. Heap
and index maintenance dominate once the policy is cheap.

**pg_auth:** **P.** Write-path checks reuse the read-path shape; the write-specific cost is
`authz.effective` maintenance (a separate benchmark), not the policy check.

---

## 22. `prepared-plan-role-revalidation` — plan cache and RLS

**Category:** perf · **Rank:** 22

**Evidence**
- `plan_cache_mode` background and generic vs custom plan behaviour (not RLS specific):
  <https://www.postgresql.org/docs/current/runtime-config-query.html>;
  <https://thebuild.com/blog/all-your-gucs-in-a-row-plan_cache_mode/> (via search).
- PgBouncer prepared-statement tracking and cross-tenant plan sharing (claim of up to 10x
  slowdown): <https://daily.dev/posts/pgbouncer-and-rls-tenant-context-set-local-shared-plans-wvpn1fj2i>
  (via search; **UNVERIFIED**).
- Source: cached plans record `dependsOnRLS` and the `rewriteRoleId`; when the executing role
  differs the plan is revalidated (src: `plancache.c` `RevalidateCachedQuery`, unverified line).
- **UNVERIFIED externally:** no RLS-specific public benchmark of this effect.

**Symptom:** PostgREST-style workloads that alternate roles (`anon`, `authenticated`) on
prepared statements re-plan more than expected; generic plans for policy queries with
subject-dependent estimates are chosen after five executions and may be wrong for the next
subject.

**Root cause:** RLS plans depend on the current role and (by design) must not be shared across
roles; policies that depend on a GUC are planned with the GUC as an unknown (issue 17) and a
generic plan fixes that guess.

**Naive:** prepared statements over RLS tables with alternating `SET LOCAL ROLE`.

**Best-practice fix:** keep the role set small and stable per connection pool; use
`plan_cache_mode = force_custom_plan` for policy-heavy hot queries (costs planning every time).

**Repro** (pgbench; the earlier scenarios' `documents` policy `org_id = ANY(ARRAY(SELECT my_orgs()))`
installed, second role `rls_app2` granted)
```sql
-- setup
DO $$ BEGIN CREATE ROLE rls_app2 NOLOGIN; EXCEPTION WHEN duplicate_object THEN NULL; END $$;
GRANT rls_app TO rls_app2; GRANT USAGE ON SCHEMA bench TO rls_app2; GRANT SELECT ON ALL TABLES IN SCHEMA bench TO rls_app2;
SELECT bench.reset();
ALTER TABLE bench.documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY d ON bench.documents FOR SELECT USING (org_id = ANY (ARRAY(SELECT bench.my_orgs())));
```
```
-- same_role.sql                         -- alt_role.sql
\set id random(1, 200000)                \set id random(1, 200000)
BEGIN;                                   \set r random(0, 1)
SET LOCAL ROLE rls_app;                  BEGIN;
SET LOCAL app.uid = 1;                   \if :r
SELECT * FROM bench.documents            SET LOCAL ROLE rls_app;
  WHERE id = :id;                        \else
COMMIT;                                  SET LOCAL ROLE rls_app2;
                                         \endif
                                         SET LOCAL app.uid = 1;
                                         SELECT * FROM bench.documents WHERE id = :id;
                                         COMMIT;
pgbench -n -M prepared -T 5 -f same_role.sql ;  pgbench -n -M prepared -T 5 -f alt_role.sql
```

**Measure:** tps and latency for same-role vs alternating roles under `-M prepared`; generic vs
custom plan counts (`pg_prepared_statements.generic_plans/custom_plans`); plan quality after a
subject change (`force_generic_plan` vs `force_custom_plan`).

**Measured (scale 1):** trivial PK lookup 13.4k tps (same role) vs 11.9k tps (alternating),
about 12% slower; the same pattern on a 16-partition table and a join: 13-15% slower. The effect
is real but small for cheap queries; I expect it to grow with planning cost (many partitions,
nested policies), not tested.

**pg_auth:** **P.** Plan caching must key on the subject (now: cached plans follow the subject's breadth, design.md); benchmark both modes
under `-M prepared` with `plan_cache_mode` variants.

---

## Coverage checklist (task families to scenarios)

| family | issue ids |
|---|---|
| per-row function calls (volatility, plpgsql, not wrapped in select) | 1, 2 |
| joins / EXISTS on membership tables | 3, 16 |
| nested RLS | 6, 11 |
| missing indexes on policy columns | 4 |
| ORDER BY ... LIMIT with selective policies | 8 |
| count(*) / aggregates over RLS tables | 3 (count), 1 |
| LIKE / ILIKE / jsonb / FTS with RLS | 7 |
| partitioned tables + RLS | 14, 15 |
| joins between two RLS tables | 16 |
| views over RLS tables | 5 |
| large IN-lists / array membership | 19 |
| role/claim checks via current_setting / JWT per row | 2, 10 |
| multi-tenant `tenant_id` policies | 17, 10 |
| hierarchical / recursive permissions | 18 |
| prepared statements / generic plans | 22, 17 |
| UPDATE / DELETE with RLS | 9, 21 |
| security: SECURITY DEFINER, leakproof, FORCE/owner, matviews, COPY/pg_dump, replication | 12, 20, 13 |

Not covered (considered, left out): full-text search `@@` with RLS (same mechanism as issue 7;
add `tsvector` + GIN to the scenario), `FOR ALL` policies and policy permissiveness
(PERMISSIVE vs RESTRICTIVE ORing: a known source of "too much access", not performance),
`INSERT ... ON CONFLICT` details (see issue 9), `row_security = off` GUC, and Realtime/logical
decoding filters.

## Appendix A. `fixture.sql`

Usage: `psql -h <host> -p <port> -d postgres -v scale=1 -f fixture.sql`. It needs a role
that can `CREATE ROLE` (it creates `rls_app`). Runs in about 1.5 s at `scale=1`.

```sql
-- fixture.sql : shared fixture. Usage: psql -v scale=1 -f fixture.sql   (scale=1 -> 200k docs, 2k orgs)
\if :{?scale} \else \set scale 1 \endif
SELECT (200000 * :scale)::int AS docs, (2000 * :scale)::int AS orgs \gset
SET max_parallel_workers_per_gather = 0;
DROP SCHEMA IF EXISTS bench CASCADE;
DO $$ BEGIN CREATE ROLE rls_app NOLOGIN NOBYPASSRLS; EXCEPTION WHEN duplicate_object THEN NULL; END $$;
CREATE SCHEMA bench;

CREATE TABLE bench.memberships (
  user_id int NOT NULL, org_id int NOT NULL, role text NOT NULL DEFAULT 'member',
  PRIMARY KEY (user_id, org_id));
CREATE INDEX ON bench.memberships (org_id);

CREATE TABLE bench.documents (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  org_id int NOT NULL, owner_id int NOT NULL,
  title text NOT NULL, body text NOT NULL, meta jsonb NOT NULL,
  created_at timestamptz NOT NULL);

CREATE TABLE bench.comments (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  doc_id bigint NOT NULL, author_id int NOT NULL, body text NOT NULL);

-- Docs are laid out in contiguous org blocks, newest block = org 1, oldest = org :orgs.
INSERT INTO bench.documents (org_id, owner_id, title, body, meta, created_at)
SELECT 1 + ((g - 1) * :orgs / :docs), 1 + g % 1000, 'doc ' || g, md5(g::text),
       jsonb_build_object('status', CASE WHEN g % 10 = 0 THEN 'draft' ELSE 'final' END, 'n', g % 100),
       now() - (g || ' seconds')::interval
FROM generate_series(1, :docs) g;
CREATE INDEX documents_org_idx ON bench.documents (org_id);
CREATE INDEX documents_created_idx ON bench.documents (created_at DESC);

INSERT INTO bench.comments (doc_id, author_id, body)
SELECT g, 1 + g % 1000, md5(g::text) FROM generate_series(1, :docs) g;
CREATE INDEX comments_doc_idx ON bench.comments (doc_id);

-- user 1 = "small" (2 oldest orgs, ~200 docs); user 2 = "power" (newest half of orgs);
-- users 3..1000 = 2 random orgs each.
INSERT INTO bench.memberships (user_id, org_id) VALUES (1, :orgs), (1, :orgs - 1);
INSERT INTO bench.memberships (user_id, org_id) SELECT 2, g FROM generate_series(1, :orgs / 2) g;
INSERT INTO bench.memberships (user_id, org_id)
SELECT u, 1 + (u * 7919 + k * 104729) % :orgs FROM generate_series(3, 1000) u, generate_series(1, 2) k
ON CONFLICT DO NOTHING;

-- identity + naive helper functions
CREATE FUNCTION bench.uid() RETURNS int LANGUAGE sql STABLE
  AS $$ SELECT nullif(current_setting('app.uid', true), '')::int $$;
CREATE FUNCTION bench.is_member_plpgsql(p_org int) RETURNS boolean LANGUAGE plpgsql STABLE
  AS $$ BEGIN RETURN EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = p_org AND m.user_id = bench.uid()); END $$;
CREATE FUNCTION bench.is_member_secdef(p_org int) RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
  AS $$ SELECT EXISTS (SELECT 1 FROM bench.memberships m WHERE m.org_id = p_org AND m.user_id = bench.uid()) $$;
CREATE FUNCTION bench.my_orgs() RETURNS SETOF int LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
  AS $$ SELECT org_id FROM bench.memberships WHERE user_id = bench.uid() $$;

-- drops every policy / disables RLS on bench tables so each scenario starts clean
CREATE FUNCTION bench.reset() RETURNS void LANGUAGE plpgsql AS $$
DECLARE r record;
BEGIN
  FOR r IN SELECT schemaname, tablename, policyname FROM pg_policies WHERE schemaname = 'bench' LOOP
    EXECUTE format('DROP POLICY %I ON %I.%I', r.policyname, r.schemaname, r.tablename);
  END LOOP;
  FOR r IN SELECT c.oid::regclass AS t FROM pg_class c
           WHERE c.relnamespace = 'bench'::regnamespace AND c.relkind IN ('r','p') LOOP
    EXECUTE format('ALTER TABLE %s NO FORCE ROW LEVEL SECURITY', r.t);
    EXECUTE format('ALTER TABLE %s DISABLE ROW LEVEL SECURITY', r.t);
  END LOOP;
END $$;

GRANT USAGE ON SCHEMA bench TO rls_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA bench TO rls_app;
VACUUM ANALYZE bench.documents; VACUUM ANALYZE bench.comments; VACUUM ANALYZE bench.memberships;
```
