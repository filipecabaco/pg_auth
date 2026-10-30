-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000).
select authz_internal.protect('app.documents_p', 'org', 'org_id', 'member');
