-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: few_orgs
-- pg_auth with a cached generic plan built for the selective subject and reused by each subject
-- (prepared statements across requests on a pooled connection).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.channels', 'org', 'org_id', 'can_read_channels');
select authz.protect('app.messages', 'org', 'org_id', 'can_read_messages');
