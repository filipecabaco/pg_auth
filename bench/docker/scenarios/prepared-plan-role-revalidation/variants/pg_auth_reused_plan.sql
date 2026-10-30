-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: selective
-- pg_auth with a cached generic plan built for the selective subject and reused by each subject.
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
select authz_internal.protect('app.documents_p', 'org', 'org_id', 'member');
