-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the user functions NOT leakproof (shared container: earlier variants changed the marking).
alter function app.spy(int) not leakproof;
alter function app.spy_i(int) not leakproof;
alter function app.to_int(text) not leakproof;
select authz.protect('app.docs', 'org', 'org_id', 'member');
