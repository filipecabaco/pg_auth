-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: selective
-- pg_auth with a cached generic plan built for one subject and reused by each subject.
select authz.protect('app.documents', 'org', 'org_id', 'member');
select authz.protect('app.comments', 'document', 'document_id', 'viewer');
select authz.protect('app.attachments', 'comment', 'comment_id', 'viewer');
