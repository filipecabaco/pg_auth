-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: small_editor
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.tasks', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz_internal.protect('app.drafts', 'tenant', 'tenant_id', 'viewer', 'editor');
select authz_internal.protect('app.projects', 'tenant', 'tenant_id', 'viewer');
