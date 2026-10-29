-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.tasks', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz.protect('app.drafts', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz.protect('app.projects', 'tenant', 'tenant_id', 'viewer');
