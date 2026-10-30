-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- protect without a write permission: writes are denied (only a select policy exists).
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
