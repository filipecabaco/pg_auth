# partitioned-rls-no-pruning

Catalog issue 15. `org_id = ANY(InitPlan array)` is not known at plan time and the sublink is never pulled
up into a join, so a tenant-partitioned table is opened in every partition even for a tiny tenant.

Data: the same rows in `app.documents_p16` (16 hash partitions) and `app.documents_p128` (128), partition
key `org_id`. Subjects: `selective` (user 1, ~2 orgs), `broad` (user 0, 50% of orgs). Scale = rows (default 1M).

Queries: `count16`/`count128` (count(*)), `recent16`/`recent128` (ORDER BY created_at DESC LIMIT 20),
`literal128` (`org_id IN (1, 3)`: the tenant literal prunes statically).

Variants: `no_rls_baseline` (explicit semi-join), `naive_rls` and `best_practice_rls` (array policy; plain RLS
cannot do better, best_practice also puts the policy on the leaves).

Expected symptom / what to look at (plan details, "parts" column = partitions scanned / total):
- naive/best_practice scan 16/16 or 128/128 for the selective subject; baseline scans 2/16
  and 2/128 for `count*` (selective: ~0.06 / 0.2 ms vs 0.07-0.8 ms) and `recent16` (0.13-0.16 ms vs ~50-60 ms,
  >300x). Planning time grows with partition count (~0.1 ms for 16 vs ~1 ms for 128 partitions).
- `recent128` selective: even the baseline opens all 128 partitions (Merge Append), so only ~1.7x better.

## pg_auth

Model: `org.member`; `documents_p16` and `documents_p128` are protected (the parent, as in org-membership) by `org_id`/`member`. Default-scale (1M rows) geomean vs baseline: pg_auth 2.1x, best_practice_rls 3.3x, naive_rls 3.2x. Whether the listed ids prune partitions at run time is not verified here; `recent16`/`recent128` for the selective subject (~50 ms) cost the same as in RLS, so no gain on that query.
