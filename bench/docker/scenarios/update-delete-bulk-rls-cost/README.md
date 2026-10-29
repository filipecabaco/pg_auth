# update-delete-bulk-rls-cost

Catalog issue 21. UPDATE evaluates the policy USING on candidate old rows and WITH CHECK on new rows
(plus the SELECT policy when it reads the row); DELETE evaluates USING. Function policies add per-row cost.

The harness runs every query under `EXPLAIN (ANALYZE)` inside a transaction that is rolled back, so DML needs
no workaround; each statement is wrapped as `with u as (update ... returning 1) select count(*) from u` so the
warm-up run returns a comparable row (rows affected) and the result-consistency check works.

Data: `app.documents` (+ memberships); statements touch `id % 2 = 0` rows (50% of the table is a candidate; for
`broad` ~25% is written, for `selective` almost nothing). Scale = rows (default 500k).

Queries: `bulk_update` (SET title), `bulk_update_by_org` (index-friendly `org_id % 4`), `bulk_delete`,
`bulk_update_returning` (needs the SELECT policy too).

Variants: `no_rls_baseline` (superuser with the filter in the statement: the floor), `naive_rls` (plpgsql membership
function in SELECT/UPDATE USING+WITH CHECK/DELETE policies), `best_practice_rls` (InitPlan array in every policy).

What to look at: naive_rls pays the per-row function for every candidate row (~0.85 s even for the selective
subject who writes almost nothing; 2.1 s vs 1.2 s for the broad bulk update). Array policies stay
within ~1-2x of the owner's statement: heap/index maintenance dominates (see "top nodes": ModifyTable).

## pg_auth

`authz.protect('app.documents', 'org', 'org_id', 'member', 'member')`: select, insert, update (USING and WITH
CHECK) and delete policies, each `authz.permits(...)`. All four pg_auth variants returned the baseline's rows
(default scale 500k, 3 reps, 2 CPU / 2 GB, indicative under load). Median ms:

| subject / query | no_rls_baseline | best_practice_rls | pg_auth | pg_auth_no_index | pg_auth_per_row | pg_auth_reused_plan |
| --- | --- | --- | --- | --- | --- | --- |
| selective / bulk_update | 0.120 | 0.109 | 0.192 | 0.072 | 457 | 0.105 |
| selective / bulk_delete | 0.067 | 0.091 | 0.088 | 0.102 | 447 | 0.217 |
| broad / bulk_update | 1136 | 2491 | 1491 | 2164 | 1423 | 1183 |
| broad / bulk_update_by_org | 55.0 | 30.6 | 493 | 1167 | 457 | 75.4 |
| broad / bulk_delete | 144 | 98.8 | 590 | 1281 | 587 | 144 |

Selective subjects (listed ids, an index condition) match RLS. The broad subject has ~2500 orgs, above the engine's
list limit, so writes keep the per-row `authz.permits` policy (design.md section 3: "writes and row locks"): where the
statement is index-friendly (`bulk_update_by_org`, `bulk_delete`) pg_auth is 5-16x slower than the array policy;
when the heap/index maintenance dominates (`bulk_update`) it is on par. `pg_auth_per_row` pays ~450 ms even for the
selective subject, which writes almost nothing.
