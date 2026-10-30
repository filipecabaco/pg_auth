-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.events', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.events_p', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.projects', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.notes', 'account', 'owner_id', 'owner');
