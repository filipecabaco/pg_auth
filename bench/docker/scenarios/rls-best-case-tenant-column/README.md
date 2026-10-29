# rls-best-case-tenant-column

The control scenario: plain RLS used the way Supabase and Postgres experts recommend, so pg_auth's
overhead is measured against RLS at its best.

## Pattern

- `app.events`: multi-tenant table, `tenant_id uuid`. Tenant 0 is large (30% of rows); the others are
  small (~350 rows at the default scale). `app.projects` (5 per tenant) is joined on the tenant key.
- `app.notes`: one owner per row, policy `owner_id = (select auth.uid())`.
- `app.events_p`: the same events hash-partitioned on `tenant_id` (16 partitions), the
  "partition-per-tenant" recommendation in its cheap form.
- Policies read the tenant from a custom JWT claim: `tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid)`.
  An alternative variant looks the tenant up in `app.members` instead of using a claim.
- Subjects: `small_tenant` (user-1-0) and `large_tenant` (user-0-0), both with `request.jwt.claims`.

## Indexes, and the one each query should use

| table | index | used by |
|---|---|---|
| events | pk `(id)` | `pk_small`, `pk_large` (policy is a filter on the fetched row) |
| events | `(tenant_id, created_at desc)` | `recent_page`: one range scan on the tenant, no sort, stops after 20 rows; `count`: index-only scan |
| events | `(tenant_id, project_id)` | `join_count`, `join_page`: the join key |
| projects | pk `(id)`, `(tenant_id)` | probe side of the join |
| notes | `(owner_id, created_at desc)` | `notes_recent`, `notes_count` |
| events_p | pk `(tenant_id, id)`, `(tenant_id, created_at desc)` | `part_recent`, `part_count`: pruned to one partition when the tenant is known at executor start |

## Why RLS is good here

The policy is an equality on the leading column of a btree index against a value the planner can
evaluate once (an InitPlan from `(select ...)`). No per-row function calls, no joins in the policy,
no helper tables. `best_practice_rls` should be within noise of `no_rls_baseline`. `naive_rls`
(unwrapped `auth.jwt()`) is also fast when an index is used, because a stable expression on an index
column is evaluated once as an index condition; it loses on seq scans and estimates (the jwt-claims
scenario shows that case). The membership variant adds a tiny InitPlan and an array-valued
`= ANY` that cannot give ordered output (so `recent_page` sorts the tenant's rows: watch the large
tenant), which is a real cost of the "no custom claim" approach.

## What pg_auth does

`authz.protect` on `tenant_id` (type `tenant`, relation `member`) and on `notes.owner_id` (type `account`,
relation `owner`; one tuple per user). Each subject has one tenant, so the engine lists one object and
the plan is `tenant_id = ANY (InitPlan)` over the same indexes. Expect a gap of a sidecar round trip
(or a cached answer) per statement plus planning time, and a sort on `recent_page` for the large tenant
if the planner cannot take the order from `= ANY` with an unknown array length. Read the plan-ms
column in the generated report for planning time per query.

## Honesty notes

- The claim-based policy is only equivalent to the OpenFGA membership because each user belongs to
  exactly one tenant and the claim names it. A user in several tenants would need `= ANY` of a claim
  array (or the members lookup) in RLS; pg_auth handles that with no change.
- pg_auth reads the same facts (members table -> tuples) so results must be identical; the runner checks.

## Sources

- Supabase, Row Level Security (performance recommendations: wrap in `select`, index policy columns, `to` role, security definer helpers): https://supabase.com/docs/guides/database/postgres/row-level-security
- Supabase, RLS performance and best practices: https://supabase.com/docs/guides/troubleshooting/rls-performance-and-best-practices-Z5Jjwv
- PostgreSQL docs, Table partitioning and partition pruning (runtime pruning with InitPlan parameters): https://www.postgresql.org/docs/current/ddl-partitioning.html
- PostgreSQL docs, Row security policies: https://www.postgresql.org/docs/current/ddl-rowsecurity.html

## Scale

`--scale` = number of events (default 200000). 10x = 2,000,000 events, 4,000 tenants, 1,000,000 notes;
data is generated with `generate_series`. OpenFGA tuples are 3 members + 3 accounts per tenant
(24,000 at 10x).
