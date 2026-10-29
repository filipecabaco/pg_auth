# membership-subplan-no-pullup (catalog #3)

Documents scoped by org membership. Subjects: `selective` (user 1, ~2 orgs) and `broad`
(user 0, half the orgs). Scale = number of documents (default 200k).

Variants
- `naive_rls`: correlated `EXISTS (SELECT 1 FROM memberships ...)` (memberships has its own RLS).
- `naive_in_rls`: `org_id IN (SELECT ...)`.
- `best_practice_rls`: `org_id = ANY (ARRAY(SELECT my_orgs()))`.
- baseline: explicit semi-join in the query text.

Expected symptom: naive plans show `Filter: (hashed SubPlan ...)` / `SubPlan` on a Seq Scan or a
walk of the `created_at` index (`recent`); `count` is O(table); estimates are far off (`by_owner`,
`recent`). The array form fixes `count` but `recent` for a selective subject still walks the sort
index in the planner's eyes.

Look at: SubPlan vs InitPlan nodes, rows removed by filter, row-estimate q-error, per-row subplan flag.

## pg_auth

Model: `org.member`; `app.documents` protected by `org_id`/`member`. The membership EXISTS/IN that stays a SubPlan in RLS becomes a plain id filter. Default-scale geomean vs baseline: pg_auth 1.3x, best_practice_rls 3.4x, naive 4.0x/4.5x.
