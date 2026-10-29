# policy-recursion-42p17 (correctness / availability)

Catalog issue 11. The membership policy reads `memberships` itself ("members see their orgs'
members"); docs additionally reads `invites` while `invites` reads `docs` (a cross-table cycle). The
correct answers (baseline) are: peers of user1's orgs, docs of user1's orgs plus invited docs,
and the invites on those docs.

| variant | verdict |
| --- | --- |
| no_rls_baseline | correct |
| naive_rls | ERROR for every query: `infinite recursion detected in policy for relation "memberships"` (42P17) |
| best_practice_rls | safe, same rows (SECURITY DEFINER helpers with fixed search_path, owner superuser, no user-id argument) |

## pg_auth

Variant `pg_auth`: `memberships` protected by `org_id` (org member), `docs` by their own id (`doc#viewer = member from
org or invitee`), `invites` by `doc_id` (visible with its doc). The tuples hold memberships, doc-org links and invites.
pg_auth policies call `authz.permits(...)`, a function over the row's own column that never reads a protected
table, so there is no policy to re-expand: no 42P17. All four queries return the baseline's rows
(identical hashes); naive_rls errors `infinite recursion detected in policy for relation "memberships"`. The
cross-table cycle (docs <-> invites) is resolved inside the engine model, not in SQL.
