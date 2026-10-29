-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('storage.objects', 'folder', 'acl_key', 'viewer');
