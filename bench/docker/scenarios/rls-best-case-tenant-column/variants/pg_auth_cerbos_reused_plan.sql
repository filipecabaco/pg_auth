-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: small_tenant
-- pg_auth_cerbos with a cached generic plan built for the small tenant and reused by each subject.
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.events', 'tenant_row', 'id', 'read', attributes => array['tenant_id']);
select authz.protect('app.events_p', 'tenant_row', 'id', 'read', attributes => array['tenant_id']);
select authz.protect('app.projects', 'tenant_row', 'id', 'read', attributes => array['tenant_id']);
select authz.protect('app.notes', 'note', 'id', 'read', attributes => array['owner_id']);
