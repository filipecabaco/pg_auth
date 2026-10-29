# large-membership-array-overhead (catalog #19)

ACL-style sharing: `app.doc_grants(user_id, doc_id)`. Subjects: `few_grants` (user 1, 10 grants),
`medium_grants` (user 5000, scale/20) and `many_grants` (user 9999, scale/2). Scale = documents
(default 200k, so up to 100k grants for one subject).

- `naive_rls`: `id = ANY (ARRAY(SELECT my_doc_ids()))` (SECURITY DEFINER helper).
- `best_practice_rls`: no materialised set; `EXISTS` probe on the grant table per row. This is a
  per-row SubPlan (index probe), good for PK lookups, poor for full counts by the many-grants user.
  (Catalog also lists "use groups to keep the set small", not expressible in this flat ACL data.)

Expected symptom: PK lookup latency scales with the subject's grant count for naive
(InitPlan builds the whole array); flat for best_practice. Look at the InitPlan node's
exclusive time, planning time, and buffers.
