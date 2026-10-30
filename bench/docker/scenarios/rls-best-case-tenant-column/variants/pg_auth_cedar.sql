-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth enforcing Cedar, run by the sidecar (pg_auth/cedar): each plan compiled into a filter on
-- tenant_id (owner_id for notes), as pg_auth_cerbos does.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub', subject_type => 'User');
\set schema `cat /scenario/pg_auth/cedar/schema.cedarschema`
\set policies `cat /scenario/pg_auth/cedar/policies.cedar`
select authz.set_rules('cedar', 'schema', :'schema');
select authz.set_rules('cedar', 'policies', :'policies');
select authz_internal.protect('app.events', 'TenantRow', 'id', 'read', attributes => array['tenant_id']);
select authz_internal.protect('app.events_p', 'TenantRow', 'id', 'read', attributes => array['tenant_id']);
select authz_internal.protect('app.projects', 'TenantRow', 'id', 'read', attributes => array['tenant_id']);
select authz_internal.protect('app.notes', 'Note', 'id', 'read', attributes => array['owner_id']);
