-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- Same as naive_rls's trap: a superuser marked the user functions LEAKPROOF, now with pg_auth's policy.
alter function app.spy(int) leakproof;
alter function app.spy_i(int) leakproof;
alter function app.to_int(text) leakproof;
select authz.protect('app.docs', 'org', 'org_id', 'member');
