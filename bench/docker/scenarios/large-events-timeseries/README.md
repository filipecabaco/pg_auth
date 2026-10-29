# large-events-timeseries

Multi-tenant `app.events` (default 5M rows over 10 days, tenant sizes skewed: tenant 1 ~8% of rows, the last tenant ~800).
Indexes: BRIN(created_at), btree(tenant_id, created_at), partial btree(created_at) where kind='error',
covering btree(created_at) include (tenant_id, kind), GIN jsonb_path_ops(payload).
Subjects: `selective` (smallest tenant), `mid` (10 tenants), `big_tenant` (tenant 1), `many` (1333 tenants, past LIST_LIMIT).
OpenFGA holds one tuple per membership (~3 500 tuples), none per event: `authz.protect('app.events','tenant','tenant_id','member')`.

Queries: `last_hour`, `day_counts` (group by kind), `payload_filter` (`payload @> ...`), `range_all` (3h window), `latest_n`, `hot_kind`.

## What it shows
- A tenant policy is an implicit `tenant_id = ANY(...)` on every query. Time-ordered indexes (BRIN, covering, partial) are ordered by time,
  not tenant, so `latest_n`/`hot_kind` for a tiny tenant either walk time order discarding other tenants' rows or use the
  (tenant_id, created_at) btree and sort. Which one the planner picks flips with tenant size (see big_tenant vs selective in the report).
- `jsonb @>` is not leakproof in PG18 (`jsonb_contains`, proleakproof = f). With a leakproof policy (`= ANY(array)`) the GIN is still usable;
  with the naive per-row plpgsql membership function the planner cannot push `@>` into the index and scans the table (12.7 s, `payload_filter`).
- pg_auth (listed ids) produces the same `= ANY(...)` shape as best_practice_rls, so it tracks it within noise; `many` (permission index,
  AuthzBatch/check) is the case to watch. `pg_auth_per_row` checks every row the scan reaches and is 10-100x slower on broad scans.

## Sources
- https://supabase.com/docs/guides/database/postgres/row-level-security (sections "Add indexes" and "Call functions with select")
- https://www.postgresql.org/docs/current/rules-privileges.html (leakproof and index selection)
- https://www.postgresql.org/docs/current/brin.html
