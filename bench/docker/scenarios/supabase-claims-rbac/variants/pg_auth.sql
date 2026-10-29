-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000): subjects with fewer objects get the
-- listed-ids plan, the others the per-row check.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.channels', 'org', 'org_id', 'can_read_channels');
select authz.protect('app.messages', 'org', 'org_id', 'can_read_messages');
