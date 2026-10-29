# non-leakproof-operators-lose-index (catalog issue 7)

**Issue.** Under RLS, a user predicate whose operator/function is not LEAKPROOF may only run after
the policy quals and cannot become an index condition. `~~` (LIKE), jsonb `@>`, `lower()` and
`numeric =` all lose their trigram / GIN / expression / btree index; `=` on text and int range
(leakproof) keep it. Cost scales with what the policy lets through, so the broad subject suffers.

**Shape.** `app.documents` (20 docs per org, `scale/20` orgs) with trigram GIN on `body`, jsonb
GIN on `meta`, btree on `lower(title)`, `title`, `price`. Queries: `like_rare`, `like_common`,
`jsonb_contains`, `lower_eq`, `numeric_eq` (non-leakproof) and `text_eq`, `id_range` (leakproof
controls). Subjects: selective (~2 orgs, ~40 rows) and broad (50% of orgs).

**Variants.**
- `no_rls_baseline`: explicit `org_id IN (...)`, every index usable.
- `naive_rls`: `EXISTS` on memberships. `best_practice_rls`: `org_id = ANY(InitPlan array)`.

**Look at.** Plan per query in the result `.md`: the policy-array filter; `Bitmap Index Scan` on the trigram/GIN index vs on
`org_id`; time ratio vs baseline for broad subject; the selective subject, where the policy index
is already so selective that the issue is invisible.

## pg_auth

Model: `org.member`; `app.documents` protected by `org_id`/`member`. Default-scale geomean vs baseline: pg_auth 3.6x, best_practice_rls 4.2x, naive_rls 25x. For the broad subject the non-leakproof user quals (like_*, jsonb, lower_eq, numeric_eq) cost the same under pg_auth as under best_practice_rls (about 7-11 ms against 0.1-0.7 ms for the baseline), so pg_auth does not escape the leakproof ordering problem there; it only avoids the naive policy cost for the selective subject. Without the permission index (pg_auth_no_index) the broad subject is 3-5x worse again.
