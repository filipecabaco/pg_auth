# join-two-rls-tables (catalog issue 16)

**Issue.** `documents JOIN comments` where both tables are RLS-protected. Each policy is an opaque
filter (default selectivity), so the planner picks join order/method on guesses: e.g. a nested loop
estimated at tens of thousands of rows that really yields hundreds, re-filtering with SubPlans.

**Shape.** `documents(org_id)` and `comments(doc_id, org_id)` (org_id denormalised, as the catalog's
fix requires), `scale` rows each. Queries: `join_agg` (group by title, top 10), `join_count`,
`join_one_doc` (point join), `join_filtered` (extra non-leakproof `LIKE` on comments). Subjects: selective (~2 orgs) and broad (50% of orgs).

**Variants.** `naive_rls` (EXISTS on memberships on both sides), `best_practice_rls`
(`org_id = ANY(array)` on both). Baseline: explicit semi-joins on both sides.

**Expected.** naive: mis-estimated joins (look at est/actual q-error at the root and worst node),
SubPlans per row, slow for the broad subject. best_practice: right order for selective, hash joins
for broad.

Run at `--scale 500000` to see it clearly (naive broad join ~240 ms, 1M buffers via nested loop vs ~100 ms
for the array form).

## pg_auth

Model: `org` with `member: [user]`; both `app.documents` and `app.comments` are protected by their (denormalised) `org_id` with `member`, so the join sees the same permission list on each side as stats-visible `org_id = ANY(...)` filters. Tuples come from `app.memberships`. Default-scale geomean vs baseline: pg_auth 1.0x, best_practice_rls 0.88x, naive_rls 4.7x (pg_auth_per_row 3.8x: the per-row check fits badly on joins).
