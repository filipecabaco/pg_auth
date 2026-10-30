-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: few_rows
-- pg_auth_cerbos with a cached generic plan built for the selective subject and reused by each
-- subject: values read at execution, rebuilt for the admin, whose plan has another shape.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.todos', 'todo', 'id', 'read', attributes => array['owner_id']);
select authz_internal.protect('app.todo_items', 'todo', 'todo_id', 'read', attributes => array['owner_id']);
