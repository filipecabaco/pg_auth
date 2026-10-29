# update-select-coupling

Catalog issue 9: a correctness matrix, latency is irrelevant. When an UPDATE/DELETE reads columns in WHERE, or
uses RETURNING / ON CONFLICT DO UPDATE, SELECT policies apply to the existing row and the SELECT policy is also
checked against the new row. No SELECT policy means silent 0-row UPDATEs; `NOT deleted` in the SELECT policy makes
soft delete fail.

Statements (each wrapped in a CTE so the harness gets a comparable result; all touch only the ~100 rows of the
subject's lowest org; the harness rolls everything back): `update_where`, `update_returning`, `soft_delete`
(`SET deleted = true`), `delete_where`, `insert_returning`, `upsert` (ON CONFLICT (id) DO UPDATE).

Variants
- `no_rls_baseline`: superuser, explicit membership filter: the expected rows affected.
- `naive_rls`: SELECT policy `NOT deleted AND org in (...)` + UPDATE/INSERT/DELETE policies (catalog case c).
  Expect `soft_delete` -> error "new row violates row-level security policy".
- `naive_rls_no_select_policy` (extra): UPDATE/INSERT/DELETE policies only (catalog cases a/b). Expect
  `update_where`, `update_returning`, `soft_delete`, `delete_where` to affect 0 rows (result mismatch vs the baseline)
  and `insert_returning`, `upsert` to raise 42501.
- `best_practice_rls`: SELECT policy covers what UPDATE reads (no deleted filter).

What to look at: errors shown as `error: ...` cells in the timing table and the "Result consistency" section
(only `naive_rls_no_select_policy` differs; the errors in the naive variants are the bug being demonstrated).

## pg_auth

`authz.protect('app.documents', 'org', 'org_id', 'member', 'member')` installs separate select, insert, update and
delete policies from the read and write permission. Variants: `pg_auth`, `pg_auth_read_only` (protect without a
write permission). Hashes vs no_rls_baseline (`--scale 5000`, both subjects):

* `pg_auth`: all six statements match the baseline, including `soft_delete`, `insert_returning` and `upsert`. The
  coupling trap does not appear because there is no `NOT deleted` in the select policy (pg_auth cannot express
  row-state predicates: hide soft-deleted rows with a view or an app filter) and the select and write policies
  are installed together.
* `pg_auth_read_only`: writes are denied: update/soft_delete/delete affect 0 rows, insert_returning and upsert
  raise 42501. Same as naive_rls_no_select_policy but by design (no write permission = denied).
