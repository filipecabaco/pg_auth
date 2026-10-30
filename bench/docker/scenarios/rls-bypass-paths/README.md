# rls-bypass-paths (correctness)

Catalog issue 13. All queries run as user1 and count the rows reached through one path
(correct: org 1 only). Roles `app_owner` (owns tables, no FORCE) and `app_bypass` (BYPASSRLS) are
reachable through `SET ROLE` inside `app.count_as()` (bench_user is granted them WITH INHERIT FALSE
so nothing is inherited accidentally).

Paths: `app_role`; `owner_role` (owner without FORCE); `bypassrls_role`; `matview` (materialized view
built over the protected table); `part_parent`, `part_leaf_direct` (leaf queried directly) and
`part_late_leaf` (partition created AFTER the policies were installed); `fk_existence_oracle`
(inserting a reference confirms hidden rows: RI checks run with owner rights).

| variant | verdict |
| --- | --- |
| no_rls_baseline | correct |
| naive_rls | UNSAFE: owner_role, bypassrls_role, matview, part_leaf_direct, part_late_leaf, fk oracle |
| best_practice_rls | safe except fk_existence_oracle (policy for all roles + FORCE, policy on every leaf incl. the late one, matview replaced by an RLS table, `nobypassrls` for the app role; BYPASSRLS role is denied here only because it cannot read `memberships`) |

Limit: pg_dump/COPY/replication paths need external tools and are not reproduced.

## pg_auth

Variants: `pg_auth` (naive layout: matview kept, BYPASSRLS role kept, partition created after `protect`),
`pg_auth_no_force` (`authz_internal.protect(..., force => false)`), `pg_auth_table_not_matview` (matview replaced by a table
protected on `org_id`, `app_bypass` made NOBYPASSRLS, like best_practice_rls). Evidence: result hashes vs
no_rls_baseline (`--scale 2000`).

| path | pg_auth | pg_auth_no_force | pg_auth_table_not_matview |
| --- | --- | --- | --- |
| owner_role | closed (FORCE is the default; the subject comes from the GUC, not from the role) | SHARED leak (owner skips RLS) | closed |
| bypassrls_role | SHARED leak (BYPASSRLS skips all policies, pg_auth installs policies) | leak | closed only because the role was altered |
| matview | SHARED leak (a materialized view cannot carry policies; its content was built without a subject) | leak | closed (table + protect) |
| part_parent / part_leaf_direct | closed (policies installed on every partition) | closed | closed |
| part_late_leaf | closed: the DDL event trigger applies the policy to a partition created after `protect` (naive_rls leaks) | closed | closed |
| fk_existence_oracle | SHARED leak: RI checks run as the table owner with RLS forced off (`SECURITY_NOFORCE_RLS`) | leak | leak |

So pg_auth closes owner/FORCE and partition paths by construction, and shares what no policy system can
reach: BYPASSRLS/superuser roles, materialized views and referential-integrity probes.
