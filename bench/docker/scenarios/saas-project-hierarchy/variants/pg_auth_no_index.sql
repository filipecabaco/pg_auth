-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
-- pg_auth with production defaults but without the permission index: subjects with too many
-- objects to list are checked with the engine (AuthzBatch).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.projects', 'project', 'id', 'viewer');
select authz.protect('app.issues', 'project', 'project_id', 'viewer');
select authz.protect('app.labels', 'project', 'project_id', 'viewer');
select authz.protect('app.comments', 'project', 'project_id', 'viewer');
select authz.protect('app.issue_labels', 'project', 'project_id', 'viewer');
