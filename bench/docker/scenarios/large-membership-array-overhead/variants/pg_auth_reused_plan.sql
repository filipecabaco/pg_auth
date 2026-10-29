-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: few_grants
-- pg_auth with a cached generic plan built for the few_grants subject and reused by each subject.
select authz.protect('app.documents', 'document', 'id', 'viewer');
