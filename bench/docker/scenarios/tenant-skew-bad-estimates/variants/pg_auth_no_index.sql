-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
-- pg_auth without the permission index: big subjects are checked with the engine (AuthzBatch).
select authz_internal.protect('app.events', 'tenant', 'tenant_id', 'member');
