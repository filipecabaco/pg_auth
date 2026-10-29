-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth enforcing Cerbos's rule on the rows' attributes (pg_auth/cerbos/todo.yaml): the plan
-- compiled into a filter on owner_id. The principal's roles come from the claim `role`.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.todos', 'todo', 'id', 'read', attributes => array['owner_id']);
select authz.protect('app.todo_items', 'todo', 'todo_id', 'read', attributes => array['owner_id']);
