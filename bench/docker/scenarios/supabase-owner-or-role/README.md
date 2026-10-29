# supabase-owner-or-role

The most common Supabase policy after "owner only": a row is visible to its owner **or** to staff
whose JWT says so.

    using (owner_id = auth.uid() or (auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'))

`app.todos` (owner_id, done, created_at) with a child `app.todo_items` (carries owner_id, as multi-tenant
schemas usually do). Subjects: `few_rows` (owns ~20 todos per 50k), `many_rows` (owns 10%), `admin`
(claims carry `app_metadata.role = admin`: sees everything). `app_metadata` is used, not `user_metadata`,
because users can edit the latter (Supabase RLS docs).

## Indexes and the plan each query should use

| index | serves |
|---|---|
| `todos(owner_id, created_at desc, id desc)` | "my newest todos" for an owner-only filter |
| `todos(created_at desc, id desc)` | `page`: newest first, stop after 20 (the right plan for a broad subject) |
| `todos(created_at desc, id desc) where not done` (partial) | `open` |
| `todos` pk | `list` (ordered by id), `pk_a`, `pk_b` |
| `todo_items(todo_id, id)` | `join` (nested loop from todos) |
| `todo_items(owner_id, id)` | the child's policy column, `items` |

## Variants

- `naive_rls`: Supabase docs copy-paste, `auth.uid()` and `auth.jwt()` bare. Both are stable functions
  re-evaluated per row (a jsonb parse each time); the child asks the parent through nested RLS.
- `best_practice_rls`: `(select auth.uid())` and `(select auth.jwt() ...)`, `to bench_user`, indexed
  columns. Both become InitPlans, but `owner_id = $0 OR $1 in (...)` is an OR of a column test and a
  row-independent test: the planner cannot use the owner_id index, so a user with 20 rows scans the table.
- `split_by_role_rls`: two permissive policies instead of one OR. Permissive policies are OR'd into
  the same qual, so nothing changes (a common attempted fix).
- `no_rls_baseline`: the filter written into each query (same OR, same InitPlan on the claims). It is
  the same plan as best_practice: the OR itself, not RLS, defeats the index. The real fix in plain SQL
  is `UNION ALL` / branching on the role outside the policy, which a policy cannot do.
- `pg_auth*`: one OpenFGA object per owner (`account:<uuid>`) with `viewer: holder or admin from app or
  support from app`; `todos` and `todo_items` are protected by `owner_id`. Tuples: two per owner plus
  the role holders, so they grow with users, not todos. The subject is the JWT `sub`.

## What to expect

RLS: fine for `many_rows`/`admin` ORDER BY LIMIT (index on created_at, stop early), 20-60x too slow
for naive on every full scan (per-row claims parsing), and a full table scan for the few-rows user even
when well written. pg_auth: the few-rows user gets the listed-ids plan and index access (the only
variant faster than the explicit baseline here); broad subjects (admin: the whole table) take the
permission-index/check path, where join-with-LIMIT shapes are the weak spot (admin `join` is a nested
loop that RLS plans for free).

## Sources

- Supabase, Row Level Security (wrapping in `select`, `app_metadata` vs `user_metadata`, `to authenticated`):
  https://supabase.com/docs/guides/auth/row-level-security
- Supabase, RLS performance and best practices (linked from the page above; the `select` wrapping
  benchmarks are in https://github.com/GaryAustin1/RLS-Performance, not re-verified here).
- Supabase, Custom claims and RBAC: https://supabase.com/docs/guides/api/custom-claims-and-role-based-access-control-rbac
- Not consulted: GitHub discussions on slow `auth.jwt()` policies (no fetch of them was done).
