# secdef-helper-oracle (correctness)

Catalog issue 12. `app.is_member_of(p_user, p_org)` is SECURITY DEFINER (RLS skipped) and trusts its
user-id argument; EXECUTE is PUBLIC. `oracle_app_helper` counts other users' memberships the caller
can discover (correct 0).

| variant | verdict |
| --- | --- |
| no_rls_baseline | correct |
| naive_rls | UNSAFE: oracle_app_helper discovers the other 9 users' memberships |
| best_practice_rls | safe (helper recreated SECURITY INVOKER; policy helper `is_member(p_org)` derives the subject) |

Harness gap: the runner re-GRANTs EXECUTE on every function in every non-system schema to
bench_user after installing a variant, so `REVOKE EXECUTE ... FROM PUBLIC` (part of the catalog fix)
cannot be demonstrated; the variant uses the SECURITY INVOKER form instead. Not covered:
search_path hijacking (the helpers pin `search_path`).

## pg_auth

`memberships` is protected as own-row (`profile:<user>` with relation `owner`, bound on `user_id`) and `docs` by org
member, so policies need no helper function at all. Variants (hashes vs no_rls_baseline, `--scale 1000`):

| variant | verdict |
| --- | --- |
| pg_auth | SHARED: `app.is_member_of` is app code, SECURITY DEFINER and owned by the superuser that ran schema.sql, so it still answers for any user (9 memberships discovered). pg_auth does not touch functions |
| pg_auth_invoker | closed: helper SECURITY INVOKER, memberships' pg_auth policy applies to the caller |
| pg_auth_owner | CLOSED even though the helper stays SECURITY DEFINER, when its owner is an ordinary role: the FORCEd policy keys on the request subject, not the executing role. A superuser or BYPASSRLS owner still bypasses |

`own_docs` and `own_memberships` match the baseline in all variants. The `REVOKE ... FROM PUBLIC` gap noted above
(harness re-grants EXECUTE) applies here too.
