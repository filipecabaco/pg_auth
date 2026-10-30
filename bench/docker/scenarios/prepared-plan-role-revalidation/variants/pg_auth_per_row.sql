-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
-- pg_auth with a sidecar that never lists: every subject is checked per row / in batches.
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
select authz_internal.protect('app.documents_p', 'org', 'org_id', 'member');
