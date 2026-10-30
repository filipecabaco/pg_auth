-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
-- pg_auth with production defaults but without the permission index: subjects with too many
-- objects to list are checked with the engine (AuthzBatch).
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
select authz_internal.protect('app.documents_p', 'org', 'org_id', 'member');
select authz_internal.protect('app.comments', 'document', 'document_id', 'viewer');
