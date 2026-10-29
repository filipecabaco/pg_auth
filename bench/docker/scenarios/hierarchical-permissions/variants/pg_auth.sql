-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000): subjects with fewer folders get the
-- listed-ids plan, the others the per-row check. No closure table: OpenFGA resolves the tree.
select authz.protect('app.files', 'folder', 'folder_id', 'viewer');
