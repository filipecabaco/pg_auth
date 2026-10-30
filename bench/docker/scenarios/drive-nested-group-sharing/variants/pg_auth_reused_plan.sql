-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: direct
-- pg_auth with a cached generic plan built for the selective subject and reused by each subject
-- (prepared statements across requests on a pooled connection).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.folders', 'folder', 'id', 'viewer');
select authz_internal.protect('app.documents', 'folder', 'folder_id', 'viewer');
