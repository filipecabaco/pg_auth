-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: small_tenant
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.events', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.events_p', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.projects', 'tenant', 'tenant_id', 'member');
select authz_internal.protect('app.notes', 'account', 'owner_id', 'owner');
