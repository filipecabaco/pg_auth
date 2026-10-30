-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth enforcing Cedar, run by the sidecar (pg_auth/cedar): the plan compiled into a filter on
-- owner_id, every row for an admin, as pg_auth_cerbos does.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub', subject_type => 'User');
\set schema `cat /scenario/pg_auth/cedar/schema.cedarschema`
\set policies `cat /scenario/pg_auth/cedar/policies.cedar`
select authz.set_rules('cedar', 'schema', :'schema');
select authz.set_rules('cedar', 'policies', :'policies');
select authz_internal.protect('app.todos', 'Todo', 'id', 'read', attributes => array['owner_id']);
select authz_internal.protect('app.todo_items', 'Todo', 'todo_id', 'read', attributes => array['owner_id']);
