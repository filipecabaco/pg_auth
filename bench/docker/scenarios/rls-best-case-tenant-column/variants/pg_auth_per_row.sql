-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.events', 'tenant', 'tenant_id', 'member');
select authz.protect('app.events_p', 'tenant', 'tenant_id', 'member');
select authz.protect('app.projects', 'tenant', 'tenant_id', 'member');
select authz.protect('app.notes', 'account', 'owner_id', 'owner');
