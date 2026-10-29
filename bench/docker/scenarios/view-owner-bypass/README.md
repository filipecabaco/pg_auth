# view-owner-bypass (correctness)

Catalog issue 5. Tables and views are owned by `app_owner` (NOBYPASSRLS, no FORCE), so a view reads
its base table with the owner's RLS exemption. Paths: owner view, security_invoker view,
security_barrier view (barrier is not invoker), invoker view over an owner view, a filtered view, and
a SECURITY DEFINER set-returning function. Each returns `(rows, distinct orgs)`; correct for user1
is only org 1 (scale/10 rows, 1 org).

| variant | verdict |
| --- | --- |
| no_rls_baseline | correct |
| naive_rls | UNSAFE: view_owner, view_barrier, view_on_view, view_filtered, secdef_function return the whole table |
| best_practice_rls | safe (`security_invoker = true` on every view, function made SECURITY INVOKER) |

## pg_auth

Variants (same views and definer function as naive_rls, `--scale 2000`, hashes vs no_rls_baseline):
`pg_auth` (objects owned by `app_owner`, NOBYPASSRLS; protect with FORCE, the default), `pg_auth_no_force`
(`force => false`), `pg_auth_superuser_owned` (views and function owned by the superuser).

| variant | view_owner / view_barrier / view_on_view / view_filtered / secdef_function |
| --- | --- |
| pg_auth | CLOSED: FORCE applies the policy to the view owner too, and the subject is the request's, not the executing role's, so owner views and definer functions only see the caller's org. No `security_invoker` needed |
| pg_auth_no_force | leak, same as naive_rls (the table owner skips the policy) |
| pg_auth_superuser_owned | SHARED leak (superusers always skip RLS, FORCE or not) |

`base_table` and `view_invoker` are correct in all variants.
