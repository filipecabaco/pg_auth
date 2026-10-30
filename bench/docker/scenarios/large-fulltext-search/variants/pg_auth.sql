-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000): subjects with fewer objects get the
-- listed-ids plan, the others the per-row check.
select authz_internal.protect('app.documents', 'tenant', 'tenant_id', 'member');
