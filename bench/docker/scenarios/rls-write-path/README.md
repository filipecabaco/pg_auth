# rls-write-path

What RLS and pg_auth do to writes: INSERT with a WITH CHECK policy, UPDATE/DELETE of the subject's
own rows (one row, batches of 100 and 1000), `INSERT ... RETURNING`, `UPDATE ... FROM` a second
protected table, and a read-only subject. Every run is rolled back.

## Pattern

`app.tasks` (tenant_id, project_id, owner_id, status) with a tenant-scoped write policy, `app.projects`
(read only, joined by UPDATE ... FROM), `app.drafts` (small, used for the row-id probe below). Reads
need membership, writes need the `editor` role (custom claim `tenant_role` in RLS; relation `editor` in
OpenFGA). Subjects: `small_editor`, `large_editor`, and `large_viewer` (read-only member of the large
tenant: every write must be denied, and the baseline guards it explicitly).

Insert queries run through `app.attempt(sql)` (plpgsql, catches 42501 and returns `'denied'`), so
every variant answers with the same text whether the policy allows or denies; that also makes any
difference in what is allowed show up in the runner's result-consistency check.

## Indexes

| index | used by |
|---|---|
| tasks pk `(id)` | `update_one/100/1000`, `delete_*` (`id in (select id ... order by id limit N)`) |
| tasks `(tenant_id, created_at desc)` | policy column leads; tenant-scoped scans |
| tasks partial `(project_id) where status = 'open'` | `update_join` (join key and filter) |
| tasks `(owner_id)` | `update_own` (`owner_id = auth.uid()`) |
| projects pk, `(tenant_id)` | join and policy |
| drafts pk, `(tenant_id)` | `insert_draft` |

Writes pay for every index: three on tasks, one of them partial and affected by `status` updates.

## How pg_auth enforces writes (docs/design.md, sql/authz.sql)

`authz.protect(table, type, column, read_permission, write_permission)` installs four policies:
`pg_auth_select` (`authz.permits(type, read, col::text)`), and, when a write permission is given,
`pg_auth_insert ... WITH CHECK`, `pg_auth_update USING + WITH CHECK`, `pg_auth_delete USING`, all with
`authz.permits(type, write, col::text)`. So INSERT has a WITH CHECK equivalent, evaluated per new row
against the engine's decision for the object named by the column; UPDATE checks old and new row, so moving
a row to a tenant the subject cannot write to is denied (`update_move_foreign`). Without a write
permission all writes are denied. For writes the planner does not use the listed-ids filter or
AuthzBatch; the per-row `permits` stays (answered from the subject's listed set, the permission index, or
one round trip per distinct id cached for the transaction).

## Finding: protecting by the row's own id cannot authorise INSERT

The natural pg_auth modelling of "each row is an object" is `authz.protect('app.drafts', 'draft', 'id', ...)`
with a tuple `draft:<id>#tenant@tenant:<t>` per row. An INSERT is checked against `draft:<new id>`, whose
tuple does not exist yet (and cannot be written atomically with the row: "Writes to the engine and to the
table do not commit atomically", design.md). The check is denied, while RLS (`tenant_id = claim`) allows it.
Variant `pg_auth_row_object` demonstrates it: `insert_draft` returns `denied` for the editors, `inserted 1`
in every other variant; the runner reports it as a result difference. The workaround is to protect by the
parent column (`tenant_id`), which is what the other pg_auth variants do. Anything whose permission can
only be derived from the new row's own id (or a tuple the application writes after the INSERT) needs a
two-step write.

## Why RLS is good/bad here

Good: one InitPlan comparison per row; `best_practice_rls` per-command policies cost close to the baseline
(with a tenant filter the planner uses the same indexes). Bad: `naive_rls` re-parses the JWT JSON per
checked row (visible on `insert_1000`, `update_1000`); the SELECT policy runs on UPDATE/DELETE targets
and on RETURNING too.

## Sources

- Supabase, Row Level Security (separate policies per command, `with check`, select-wrapping): https://supabase.com/docs/guides/database/postgres/row-level-security
- Supabase, RLS performance and best practices: https://supabase.com/docs/guides/troubleshooting/rls-performance-and-best-practices-Z5Jjwv
- PostgreSQL docs, CREATE POLICY (USING vs WITH CHECK, which policies apply to INSERT/UPDATE/RETURNING): https://www.postgresql.org/docs/current/sql-createpolicy.html

## Scale

`--scale` = number of tasks (default 20000). 10x = 200,000 tasks, 400 tenants. Drafts are capped at 5000 and
tuples grow only with tenants (3 members each) and drafts, never with tasks.
