# order-by-limit-selective-policy (catalog issue 8)

**Issue.** The policy is an opaque filter (default 50% selectivity). For `ORDER BY created_at DESC
LIMIT 20` the planner assumes an index walk on `created_at` ends after ~40 rows and picks it. A
subject whose visible rows are old makes it walk (almost) the whole index; the opposite plan
("filter, then sort") hurts broad subjects.

**Shape.** `app.documents` where each org owns a contiguous time block (org 1 newest). Subjects:
`selective` = only the oldest org (worst case for DESC walk), `mid` = every 20th org, `broad` =
newest half. Queries: `recent` (DESC LIMIT 20), `recent_page` (OFFSET 100), `oldest` (ASC LIMIT 20: the reverse direction; the selective subject is now cheap and the broad
subject, whose rows start mid-table, is the one that can walk far).

**Variants.** `naive_rls` (EXISTS), `best_practice_rls` (`= ANY(InitPlan array)`, the catalog's partial
fix). Baseline has the same two indexes and an explicit `org_id IN (...)`: it shows what the planner
does when it can see the real selectivity.

**Expected.** naive/best_practice: `selective recent` slow (`Rows Removed by Filter` ~ table size,
estimate rows far above actual, `created_at` index walk); `broad recent` fast. Look at the root and worst-node row estimate error and which index is
used in the per-query plan table.
