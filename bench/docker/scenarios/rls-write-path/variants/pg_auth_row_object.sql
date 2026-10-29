-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- Drafts are protected by the row's own id (type draft, tuple draft:<id>#tenant): the shape that fits "each row is an object". A new row has no tuple yet, so INSERT is denied.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.tasks', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz.protect('app.drafts', 'draft', 'id', 'viewer', 'editor');
select authz.protect('app.projects', 'tenant', 'tenant_id', 'viewer');
