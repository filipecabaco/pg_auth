-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
-- pg_auth with a sidecar that never lists: every row is checked (one round trip per distinct id,
-- cached for the transaction).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.channels', 'org', 'org_id', 'can_read_channels');
select authz_internal.protect('app.messages', 'org', 'org_id', 'can_read_messages');
