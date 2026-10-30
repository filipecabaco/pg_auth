-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
-- pg_auth with a sidecar that never lists: every row is checked (one round trip per distinct id,
-- cached for the transaction).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.folders', 'folder', 'id', 'viewer');
select authz_internal.protect('app.documents', 'folder', 'folder_id', 'viewer');
