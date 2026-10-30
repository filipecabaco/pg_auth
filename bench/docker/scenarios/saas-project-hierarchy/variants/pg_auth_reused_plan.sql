-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: guest
-- pg_auth with a cached generic plan built for the selective subject and reused by each subject
-- (prepared statements across requests on a pooled connection).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.projects', 'project', 'id', 'viewer');
select authz_internal.protect('app.issues', 'project', 'project_id', 'viewer');
select authz_internal.protect('app.labels', 'project', 'project_id', 'viewer');
select authz_internal.protect('app.comments', 'project', 'project_id', 'viewer');
select authz_internal.protect('app.issue_labels', 'project', 'project_id', 'viewer');
