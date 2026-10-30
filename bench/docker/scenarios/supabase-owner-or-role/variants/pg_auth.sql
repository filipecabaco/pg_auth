-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000): subjects with fewer objects get the
-- listed-ids plan, the others the per-row check.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.todos', 'account', 'owner_id', 'viewer');
select authz_internal.protect('app.todo_items', 'account', 'owner_id', 'viewer');
