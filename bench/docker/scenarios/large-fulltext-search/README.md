# large-fulltext-search

`app.documents` (default 2M rows; 1.1 GB heap, GIN tsvector 200 MB, GIN pg_trgm on title 115 MB), tenants = scale/2000, words drawn from a
skewed distribution of 100000 terms (term0 in ~35% of documents, term99990 in a handful). Subjects: `selective` (1 tenant), `mid` (10),
`many` (667 tenants), `very_many` (833). Protect: `tenant_id` -> `tenant#member`; OpenFGA has ~1 700 tuples, none per document.
Queries: `tsv @@ to_tsquery` rare/mid/common, `title ILIKE '%term%'` rare/common, and `ORDER BY ts_rank ... LIMIT 20`.

## Leakproof status (PG18.6, checked in the container)
`ts_match_vq/tq/tt` (`@@`), `textlike` (`~~`), `texticlike` (`~~*`), `jsonb_contains` (`@>`) are all NOT leakproof; `int4eq`, `texteq`, `timestamptz_lt/eq` are.
The PG docs say an index scan is not chosen on RLS tables when a WHERE operator of the index's family is not leakproof.
What the run shows: that only bites when a *non-leakproof policy qual* exists. The naive plpgsql membership function is not leakproof, so every
search is a Seq Scan (5.7 s). A policy built from leakproof pieces (`tenant_id = ANY(array)`, which is what pg_auth's listed-ids plan emits)
leaves GIN usable, but the planner then often prefers the tenant index + filter over the GIN for broad subjects (`many`: 260-720 ms, the baseline
with an explicit semi-join: 0.16-1784 ms depending on term frequency; the baseline wins by 1000x on rare terms, loses on common ones).
pg_auth only runs leakproof quals before its check; `pg_auth_per_row` (AuthzBatch) falls back to a Seq Scan with batch checks.

## Findings
- pg_auth (listed ids and permission index) matches best_practice_rls on every query; the gap to the baseline for broad subjects on rare
  terms is the planner's choice of the tenant index, not the authorization check.
- BUG (pg_auth): in `pg_auth_per_row` (AuthzBatch), `rank_top20` returns different rows: the scan's `id` comes back NULL when the target list
  has an expression over a column. Repro on this scenario: `select id, length(title) from app.documents order by id limit 2;` as any subject
  with `pg_auth.sidecar_socket=/run/pg_auth/per_row.sock` returns NULL, NULL. The runner flags it in "Result consistency"; left in on purpose.

## Sources
- https://www.postgresql.org/docs/current/rules-privileges.html
- https://www.postgresql.org/docs/current/pgtrgm.html , https://www.postgresql.org/docs/current/textsearch-indexes.html
- https://supabase.com/docs/guides/database/postgres/row-level-security (sections "Add indexes" and "Call functions with select")
