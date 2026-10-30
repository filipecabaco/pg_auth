-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
-- pg_auth with a sidecar that never lists: every row is checked (one round trip per distinct
-- folder, cached for the transaction).
select authz_internal.protect('app.files', 'folder', 'folder_id', 'viewer');
