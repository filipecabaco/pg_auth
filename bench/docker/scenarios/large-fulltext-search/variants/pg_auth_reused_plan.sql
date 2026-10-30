-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: selective
-- pg_auth with a cached generic plan built for the selective subject and reused by each subject
-- (prepared statements across requests on a pooled connection).
select authz_internal.protect('app.documents', 'tenant', 'tenant_id', 'member');
