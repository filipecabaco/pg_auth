# partition-direct-access

Catalog issue 14. Policies belong to the table named in the query, and the harness (like a blanket
`GRANT ... ON ALL TABLES IN SCHEMA`) grants `bench_user` the leaf partitions too. Querying
`app.documents_p_2` directly therefore skips the policy that protects `app.documents_p`.

Data: `app.documents_p`, 16 hash partitions on `org_id`; `app.memberships` as in org-membership.
Subjects: `selective` (user 1, ~2 orgs) and `broad` (user 0, half of the orgs). Scale = documents (default 200k).

Queries: `via_parent` (count through the parent), `leaf_2`, `leaf_11` (count straight from a leaf),
`leaf_ids` (20 ids from leaf 5).

Variants
- `no_rls_baseline`: superuser, explicit semi-join in *every* query including the leaf ones, i.e. the correct answer.
- `naive_rls`: policy on the parent only. **Expected result mismatch** on `leaf_*` (the leaf returns
  every row of the partition): this mismatch is the bug being demonstrated, not a harness problem.
- `best_practice_rls`: the same policy on the parent and on every leaf.
- `naive_leaf_rls_no_policy` (extra): RLS enabled on leaves without a policy = default deny, leaves return 0 rows
  (differs from the baseline in the other direction; "safe" but broken).

What to look at: the "Result consistency" section (naive_rls and naive_leaf_rls_no_policy differ on leaf
queries; all others identical), and that leaf timings for naive_rls are *not* a win (it returns more rows).
Latency is secondary; correct variants stay within ~1-3x of the baseline.

## pg_auth

`authz_internal.protect('app.documents_p', 'org', 'org_id', 'member')` installs the policy on the parent and on every
partition (now and when created or attached later), so leaf queries are protected without per-leaf setup. All four
pg_auth variants (`pg_auth`, `pg_auth_no_index`, `pg_auth_per_row`, `pg_auth_reused_plan`) returned the baseline's
rows for every query and subject, including `leaf_*` (default scale, 3 reps, 2 CPU / 2 GB, indicative under load).
Median ms:

| subject / query | best_practice_rls | pg_auth | pg_auth_no_index | pg_auth_per_row | pg_auth_reused_plan |
| --- | --- | --- | --- | --- | --- |
| selective / via_parent | 0.093 | 0.147 | 0.183 | 10.6 | 0.228 |
| selective / leaf_2 | 0.024 | 0.032 | 0.040 | 0.871 | 0.070 |
| broad / via_parent | 11.5 | 19.7 | 47.4 | 19.2 | 17.4 |
| broad / leaf_2 | 0.602 | 1.4 | 3.1 | 1.1 | 1.1 |
| broad / leaf_ids | 0.501 | 1.1 | 4.0 | 0.968 | 1.0 |

naive_rls leaks (differs from baseline on every leaf query); pg_auth closes the trap by construction and costs
~1.3-2x of the best RLS shape on broad reads here (broad = 1000 of 2000 orgs).
