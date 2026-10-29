# pool-session-state-leak (correctness) and subject spoofing

Catalog issue 10. A pooler cannot run inside the harness, so one request sequence is replayed on a
persistent dblink connection ("pooled server connection": opened once per call, reused by request A
and request B, no reset in between). Every query returns the rows visible to the request under
test; correct is 0 for anonymous / spoofing requests.

* `session_set_then_anon`: A uses session-level SET, B is anonymous (set nothing).
* `set_local_then_anon`: A uses SET LOCAL in a transaction; B is anonymous afterwards.
* `set_local_ok`: A inside its transaction (must see user1's rows).
* `local_outside_tx`: SET LOCAL without a transaction (no-op) then the request runs.
* `fresh_anon`: brand new connection, identity never set.
* `spoof_org2`: a non-superuser overwrites the identity GUCs (no pooler needed).

| variant | session_set_then_anon | set_local_then_anon | local_outside_tx / fresh_anon | spoof_org2 |
| --- | --- | --- | --- | --- |
| no_rls_baseline | 0 | 0 | 0 | 0 |
| naive_rls (`current_setting('request.uid')::int`) | LEAKS user1's rows | ERROR `invalid input syntax for type integer: ""` | ERROR (`""` / unrecognized configuration parameter) | LEAKS |
| best_practice_rls (SET LOCAL + `nullif(current_setting(k,true),'')::int`) | LEAKS (app bug) | safe (0) | safe (0) | LEAKS |

Fail-closed behaviour is good: unset / '' / SET-LOCAL residue yields NULL subject and 0 rows, not an
error. Session-level SET leaking across pooled requests is an application bug no database setting
can detect.

Limits: no real PgBouncer (transaction-mode plan sharing and prepared-statement effects are not
reproduced); the dblink session stands in for a server connection, opened as the superuser then
`SET ROLE bench_user`; the best-practice policy cannot protect against a role that may SET the GUC.

## pg_auth

Variant `pg_auth` replays the same sequences with identity in `pg_auth.subject_id` (functions `app.pa_*` in
`variants/pg_auth.sql`, queries in `variants/pg_auth.queries.sql`). The GUC is superuser-only, so it is set by the
"trusted layer": the dblink connection runs `reset role; set [local] pg_auth.subject_id = '1'; set role bench_user`,
while the request role `bench_user` is not granted `SET ON PARAMETER pg_auth.subject_id` (production grants it to
the API/connection role only).

| query | pg_auth verdict |
| --- | --- |
| session_set_then_anon | SHARED leak: a session-level SET by the trusted layer survives on the pooled connection and the next anonymous request inherits subject 1 (same app bug as RLS; pg_auth cannot know the request ended) |
| set_local_then_anon | closed, 0 rows: after COMMIT the GUC is unset, and "no subject: no rows" (design.md section 5), with no error (naive_rls errors on `''`) |
| set_local_ok | correct, user1's rows |
| local_outside_tx / fresh_anon | closed, 0 rows, no error (naive_rls errors) |
| spoof_org2 | CLOSED: `set_config('pg_auth.subject_id', ...)` by bench_user is denied (42501, caught in `pa_spoof`), the statement keeps subject user 1 and sees 0 org-2 rows. RLS on `request.uid` is spoofable by any role; `request.jwt.claims` is ignored with `subject_source = guc` |

Caveat: if the SET privilege is granted to the role that runs user SQL, that role can spoof. Grant it to the
connection/authenticator role and set the GUC before `SET ROLE`. A pooler that does not reset session state
still leaks any identity GUC; use `SET LOCAL` or `DISCARD ALL`.
