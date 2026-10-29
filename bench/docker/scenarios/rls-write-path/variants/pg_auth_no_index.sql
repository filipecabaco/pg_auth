-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.tasks', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz.protect('app.drafts', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz.protect('app.projects', 'tenant', 'tenant_id', 'viewer');
