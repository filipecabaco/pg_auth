# jwt-claims-parsed-per-row (catalog #2)

Policy reads the identity out of a JSON claims GUC: `owner_id = (current_setting('request.jwt.claims')::jsonb ->> 'sub')::int`.
The claims value here is ~2 KB (padded), set per variant with `-- @set: request.jwt.claims=...`
(the harness only sets subject GUCs to the bare subject id, so claims are fixed to sub=1; this
scenario therefore has a single subject, user 1, who owns ~1000 documents). `owner_id` is
deliberately unindexed.

Variants: `naive_rls` (per-row parse), `best_practice_rls` (`(select ...)` InitPlan),
`naive_indexed_rls` (control: with an index on owner_id the naive policy is fast).

Expected symptom: naive `count`/`recent` = Seq Scan whose Filter contains the jsonb cast; time is
rows x claim size and grows linearly with `--scale`; `pk` is fast. Fix = InitPlan (100x+ faster).

Look at: scan node exclusive time, `Filter:` text vs `InitPlan`.

## pg_auth

Model: `account.member`, owner n is a member of `account:n`; `app.documents` protected by `owner_id` with the `account` type. The pg_auth variants set the same padded `request.jwt.claims` as the RLS variants and read the subject with `authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub')`, so the claims are parsed once per statement, not per row. They drop `documents_owner_idx` first (naive_indexed_rls leaves it behind) to keep the no-index premise. Default-scale geomean vs baseline: pg_auth 1.3x, best_practice_rls 0.74x, naive_rls 9.0x, naive_indexed_rls 0.09x.
