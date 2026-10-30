-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: big
-- pg_auth with a cached generic plan built for one subject and reused by each subject.
select authz_internal.protect('app.events', 'tenant', 'tenant_id', 'member');
