-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
-- pg_auth with a sidecar that never lists: every row is checked.
select authz.protect('app.events', 'tenant', 'tenant_id', 'member');
