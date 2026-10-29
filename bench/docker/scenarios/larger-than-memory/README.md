# larger-than-memory

`app.big` default 20M rows: 2.2 GB heap + 1.0 GB indexes = 3.3 GB against shared_buffers=1GB (container 4 GB, so the OS page cache can still
hold much of it: "read" counts pages not in shared_buffers, not necessarily disk). Schema load 37 s. Rows of a tenant are scattered over the whole heap
(tenant = hash of id), 4000 tenants. Subjects: `selective` (1 tenant, 5000 rows), `medium` (10), `broad` (2000 tenants).
Queries: `point` (pk), `range_page` (`id >= 1000 order by id limit 100`), `tenant_page`, `full_count` (index-only scan, fits the pool after warmup),
`heap_sum` (`sum(val)`: reads the heap, shows reads in hit/read).

## What it shows
- Selective subject, `range_page`: RLS and pg_auth walk the pk index discarding other tenants' rows (6 460 buffers, ~18-20 ms) while the explicit
  baseline joins from the tenant index (1.5 ms).
- Broad subject `heap_sum`: every variant reads ~160-330k pages; the check cost sits on top (pg_auth permission index 1.7 s vs 1.3-1.8 s;
  `pg_auth_no_index` 5.9 s because 4000 ids go to the engine). Reads, not authorization, dominate when the table is cold.
- Naive per-row plpgsql: 47 s for a count, timeouts for sums.

## Sources
- https://supabase.com/docs/guides/database/postgres/row-level-security (sections "Add indexes" and "Call functions with select")
- https://www.postgresql.org/docs/current/rules-privileges.html
