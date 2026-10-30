-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
-- pg_auth with production defaults but without the permission index.
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
select authz_internal.protect('app.documents_p', 'org', 'org_id', 'member');
