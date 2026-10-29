# prepared-plan-role-revalidation

Catalog issue 22. RLS plans depend on the executing role; a cached plan is invalidated and re-planned when
the role differs from the one it was planned for (`dependsOnRLS` / `rewriteRoleId`), so PostgREST-style
alternation of roles on prepared statements re-plans more than expected.

**Harness gap and workaround.** The runner executes one statement per query (extended protocol, unnamed
statement, `EXPLAIN ANALYZE` prefix) and cannot PREPARE/EXECUTE across statements or switch roles between
executions, so protocol-level prepared statements are not exercised. The scenario instead uses PL/pgSQL
functions (`app.loop_flat`, `app.loop_part`, created in schema.sql): the static `select count(*) ... where id = $i`
inside the loop is a cached plan (custom for 5 runs, then possibly generic), and the loop does
`SET LOCAL ROLE bench_a` every iteration (`same_*`) or alternates `bench_a`/`bench_b` (`alt_*`). The SET ROLE
cost is identical in both. Timing is the whole 500-iteration loop (divide by 500 for per-call cost). No
generic/custom plan counters are available (no pg_prepared_statements for PL/pgSQL plans).

Data: `app.documents` and a 64-way hash partitioned `app.documents_p` (planning cost grows with partition count).
Policies have no `TO` clause so they apply to both roles. Scale = rows (default 50k).

Variants: `no_rls_baseline` (floor; no policies, so plans are role independent; the explicit-filter query is a set-based
equivalent, not a loop), `naive_rls` (plpgsql function policy), `best_practice_rls` (array),
plus extra `best_practice_rls_force_custom` (`plan_cache_mode=force_custom_plan`).

Observed (scale 50k, 500 iterations): flat table same-role ~2-4 ms vs alternating 11-14 ms (~5x; ~20 us -> ~25 us/call
extra replan); 64 partitions same-role ~50 ms vs alternating ~250 ms (0.1 -> 0.5 ms per call). force_custom_plan removes
the penalty of role alternation (alt_part ~170 ms) but makes `same_*` slower (~170 ms) since it plans every time.

## pg_auth

Policies without `TO` (`authz.protect` on `app.documents` and `app.documents_p`) apply to `bench_a` and `bench_b`;
the subject (`pg_auth.subject_id`) is set before the role switch and survives it, so alternating roles keep the
same subject. All pg_auth variants returned the baseline's rows. Median ms for the 500-iteration loops (default
scale 50k, 3 reps, 2 CPU / 2 GB, indicative under load):

| subject / query | best_practice_rls | best_practice_rls_force_custom | pg_auth | pg_auth_per_row | pg_auth_reused_plan |
| --- | --- | --- | --- | --- | --- |
| selective / same_flat | 2.6 | 10.1 | 3.5 | 2.3 | 4.4 |
| selective / alt_flat | 13.7 | 13.1 | 12.4 | 11.3 | 19.2 |
| selective / same_part | 52.5 | 184 | 117 | 79.8 | 161 |
| selective / alt_part | 280 | 184 | 400 | 306 | 515 |
| broad / same_part | 62.4 | 193 | 275 | 88.3 | 355 |
| broad / alt_part | 275 | 184 | 566 | 356 | 656 |

Role alternation re-plans exactly as with RLS (alt is ~4-5x same on the flat table). On 64 partitions pg_auth's
replans cost more than RLS's (planner hook plus a sidecar call per planned statement; the cached plan itself needs
none): 1.5-2x in alt_part and up to 4x in same_part for the broad subject (cause not investigated).
