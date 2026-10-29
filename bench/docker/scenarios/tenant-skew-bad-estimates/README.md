# tenant-skew-bad-estimates (catalog issue 17)

**Issue.** `USING (tenant_id = (SELECT nullif(current_setting(...),'')::int))` turns the tenant into
an InitPlan parameter. At plan time its value is unknown, so selectivity is `1/ndistinct`
(~scale/2000 rows) and the MCV list is ignored, although tenant 1 owns 50% of rows. Skew is the
norm in multi-tenant data. The wrap is itself the standard fix for per-row evaluation (issues 1-2).

**Shape.** `app.events` (tenant 1 = 50%, 2 = 10%, 3 = 5%, ~2000 tiny tenants share 35%) with indexes
on `(tenant_id, ts desc)`, `kind`, `device_id`, plus `app.devices` for a join above the filter.
Subjects: `big` (tenant 1), `medium` (tenant 2), `small` (tenant 77). Subject id == tenant id.
Queries: `count_all`, `kind_recent` (ORDER BY ts DESC LIMIT 50 with `kind = 7`, the BitmapAnd /
wrong-index case), `kind_count`, `join_devices` (compounding error through a join), `latest`.

**Variants.**
- `no_rls_baseline`: literal `tenant_id = <id>`, MCV used.
- `naive_rls`: the wrapped InitPlan policy (what the catalog calls the naive policy).
- `best_practice_rls`: no real fix exists in plain RLS; this is the closest alternative, an inlined
  STABLE function with no `(select ...)` wrap, whose value the planner does see. Not recommended in
  general (per-row evaluation risk) but shows what correct estimates would give.

**Expected.** naive: est rows ~200 vs actual ~200000 for `big` (q-error ~1000), plan shape wrong for
`big` (nested loop with many probes, BitmapAnd, index walk), `small` fine. Look at "est/actual
(q-err root/max)" and the top nodes for `big`/`medium` in each variant.

**Observed (scale 400000).** naive `big kind_recent`: root est 4 vs 50, worst node q-error 995 (BitmapAnd
on `kind` + policy, 2.9 ms vs 0.1 ms literal).

## pg_auth

Model: `tenant.member`; `tenant_users` gives user n membership in tenant n; `app.events` protected by `tenant_id`/`member`. The tenant reaches the planner as a value, so estimates are right (MCV used) like best_practice_rls. Default-scale geomean vs baseline: pg_auth 5.9x, best_practice_rls 1.4x, naive_rls 2.0x: this is a scenario pg_auth loses. The loss is concentrated in `latest` (`order by ts desc limit 50`): 24 ms (big), 7.6 ms (medium) against 0.012-0.02 ms for RLS and the baseline. The plan shows an Index Scan plus a Sort and a Custom Scan instead of the ordered `(tenant_id, ts desc)` scan with an early stop, i.e. the one-element `tenant_id = ANY(..)` filter loses the index order that `tenant_id = $1` keeps (finding, see the report).
