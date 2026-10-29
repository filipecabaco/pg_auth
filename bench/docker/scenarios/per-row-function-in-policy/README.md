# per-row-function-in-policy (catalog #1)

`app.documents` (org-scoped) with a `memberships` table. Subjects: `selective` (user 1, ~2 orgs),
`broad` (user 0, half the orgs). Scale = number of documents (default 200k).

Variants
- `naive_rls`: `USING (app.is_member_plpgsql(org_id))`.
- `naive_secdef_rls`: same with a SQL SECURITY DEFINER helper (also not inlinable).
- `best_practice_rls`: `org_id = ANY (ARRAY(SELECT app.my_orgs()))` (InitPlan, indexable).

Expected symptom: naive = Seq Scan, `Filter: is_member_plpgsql(org_id)`, huge Rows Removed by Filter;
cost O(table) regardless of subject (count / recent / by_org all scan everything; `pk` is fine
because the index narrows first). Time grows linearly with `--scale`. The function cost is not a
SubPlan in EXPLAIN; look at the scan node's exclusive time.

Look at: exclusive time of the scan node, rows removed by filter, InitPlan vs filter in the best
practice plan.

## pg_auth

Model: `org.member`; `app.documents` protected by `org_id`/`member`; the per-row helper function disappears. Default-scale geomean vs baseline: pg_auth 1.5x, best_practice_rls 3.0x, naive_rls 23x (worst cell 13000x), naive_secdef 22x. pg_auth_per_row (3.2x, worst 374x) shows what pg_auth costs when it must check per row instead of listing.
