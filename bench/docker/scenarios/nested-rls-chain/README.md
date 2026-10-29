# nested-rls-chain (catalog issue 6)

**Issue.** A policy on a child table reads its parent, which has its own policy, which reads
`memberships` (also with RLS). Postgres expands the inner policies too: SubPlan inside SubPlan,
no layer is pulled up.

**Shape.** `attachments -> comments -> documents -> memberships`, 1:1:1 rows. Queries count each
level (`l1_docs`, `l2_comments`, `l3_attachments`) so the cost per extra level is visible, plus a
pk lookup and an `ORDER BY id DESC LIMIT 20` at level 3.

**Variants.**
- `naive_rls`: nested `EXISTS` at every level.
- `best_practice_rls`: flat `col = ANY((select helper()))` per table, SECURITY DEFINER helpers
  build the arrays without the inner RLS. Cost: the id array for a broad subject is huge
  (rebuilt per statement), and each helper re-reads the previous level.

**Expected symptom.** naive: time grows with depth, selective subject is hit by per-row SubPlans
and broad subject by large nested scans. Compare `per-row subplan` and top nodes in the result
`.md`. best_practice: flat, but array building dominates for broad.

**Look at.** SubPlan nesting in the plan, `l3_*` vs `l1_docs` ratio.

## pg_auth

Model: `org.member`, `document.viewer = member from org`, `comment.viewer = viewer from document`. `documents` is protected by `org_id`/`member`, `comments` by `document_id`/`viewer`, `attachments` by `comment_id` (a comment)/`viewer`; OpenFGA resolves the whole chain, so each table is one flat id filter instead of nested policies. Tuples: memberships plus `document#org` and `comment#document`. Default-scale geomean vs baseline: pg_auth 0.72x (beats the baseline on the broad subject), best_practice_rls 3.9x (l3_pk 12 ms for the broad subject: it builds the id arrays), naive_rls 6.0x. Without the permission index (pg_auth_no_index) the broad subject falls to the engine and is far worse (3.5 s on l2_comments), so the index matters most here.
