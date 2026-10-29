# leaky-function-and-leakproof-marking (correctness)

Catalog issue 20. A user-supplied function must never run on rows the policy hides. `app.spy(org)`
counts every call on a row outside the caller's org in a GUC; each query returns `(visible, leaked)`
and the correct answer is `leaked = 0`. The baseline fences the explicit filter with `OFFSET 0`.
`cast_error` / `udf_error` use the error channel (a cast of a hidden row's text raises and quotes it).
Queries cover the spy in WHERE, JOIN ON, a CTE, a security_invoker view, an IN-subquery, a subquery
target list, ORDER BY (+LIMIT), GROUP BY, LATERAL, an aggregate FILTER, and the top-level target list.
The org_id index is intentionally absent: with it, the policy becomes an index condition and
the leak is hidden.

Variants: `naive_rls` = plain RLS + the functions marked LEAKPROOF by a superuser (the "perf hack");
`best_practice_rls` = same policy, functions NOT leakproof.

Results (scale 2000, user1; hash differs from baseline = leak):

| variant | verdict |
| --- | --- |
| no_rls_baseline | correct (leaked 0) |
| naive_rls | UNSAFE: where_clause, join_on, cte, view, subq_in leak; udf_error raises `invalid input syntax for type integer: "hidden-1"` |
| best_practice_rls | safe |

Harness note: the consistency check lists only hash differences; queries that raise an error appear
as `error: ...` in the pivot table, not in the consistency list.

## pg_auth

Variants `pg_auth` (functions not leakproof) and `pg_auth_leakproof` (functions marked LEAKPROOF by a
superuser, as in `naive_rls`). `authz.protect('app.docs', 'org', 'org_id', 'member')`; subject `pg_auth.subject_id`.
pg_auth installs an ordinary policy and only rewrites it into a filter inside the security quals
(docs/design.md section 3: "RLS's ordering still keeps non-leakproof user functions away from rows the subject may
not see"), so Postgres's own barrier rules apply unchanged.

| variant | verdict |
| --- | --- |
| pg_auth | closed: same hash as no_rls_baseline for every query (leaked = 0, no error) |
| pg_auth_leakproof | SHARED: where_clause, join_on, cte, view, subq_in leak and udf_error raises `invalid input syntax for type integer: "hidden-1"`, exactly the queries that fail under naive_rls |

pg_auth cannot defend against a LEAKPROOF marking: that attribute tells the planner the function may run before
any security qual, whatever produced the qual. The fix is the same as for RLS (never mark such functions leakproof).
