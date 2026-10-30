-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: selective
-- pg_auth with a cached generic plan built for one subject and reused by each subject.
-- Premise of this scenario: no index on org_id (an earlier variant may have created one).
drop index if exists app.documents_org_idx;
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
