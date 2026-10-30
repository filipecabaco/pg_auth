-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
-- pg_auth with production defaults but without the permission index: subjects with too many
-- objects to list are checked with the engine (AuthzBatch).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.channels', 'org', 'org_id', 'can_read_channels');
select authz_internal.protect('app.messages', 'org', 'org_id', 'can_read_messages');
