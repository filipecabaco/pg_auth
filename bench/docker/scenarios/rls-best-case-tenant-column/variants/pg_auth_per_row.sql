-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.events', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.events_p', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.projects', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.notes', 'account', 'owner_id', 'owner');
