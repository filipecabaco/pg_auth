-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
-- pg_auth with production defaults but without the permission index: subjects with too many
-- objects to list are checked with the engine (AuthzBatch).
select authz.protect('app.big', 'tenant', 'tenant_id', 'member');
