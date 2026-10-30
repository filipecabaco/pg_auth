-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.tasks', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz_internal.protect('app.drafts', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz_internal.protect('app.projects', 'tenant', 'tenant_id', 'viewer');
