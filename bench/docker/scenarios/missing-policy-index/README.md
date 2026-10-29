# missing-policy-index (catalog #4)

Same org/membership model as `membership-subplan-no-pullup`, but `app.documents(org_id)` has no
index in the base schema. Variants toggle it explicitly (each variant drops or creates
`app.documents_org_idx` first, because container state persists between variants).
Scale = number of documents (default 500k).

- `naive_rls`: policy `org_id = ANY(ARRAY(SELECT my_orgs()))`, no index on `org_id`.
- `best_practice_rls`: same policy plus `CREATE INDEX ON documents(org_id)`.
- baseline: semi-join with the index present.

Expected symptom: `count` and `list` are Seq Scans for the naive variant regardless of how few
rows the subject can see. `pk` stays fast (the PK index narrows first). With the index, the
selective subject drops to a Bitmap/Index scan; the broad subject (half the table) gains little
because a seq scan is a reasonable plan there.

Look at: scan node type, rows removed by filter, buffers.

## pg_auth

Model: `org.member`; `app.documents` protected by `org_id`/`member`. pg_auth also filters on `org_id`, so it needs the same index as plain RLS: the `pg_auth*` variants therefore drop `documents_org_idx` first (a previous variant may have left it) to keep the scenario premise, and the extra `pg_auth_indexed` variant adds it back. Default-scale geomean vs baseline: pg_auth 7.0x without the index, pg_auth_indexed 2.1x, best_practice_rls 4.7x (indexed), naive_rls 26x. The unindexed pg_auth count for the selective subject is a Seq Scan (17 ms vs 0.04 ms indexed): the pg_auth plan does not remove the need to index the protected column.
