-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: few_rows
-- pg_auth with a cached generic plan built for the selective subject and reused by each subject
-- (prepared statements across requests on a pooled connection).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.todos', 'account', 'owner_id', 'viewer');
select authz.protect('app.todo_items', 'account', 'owner_id', 'viewer');
