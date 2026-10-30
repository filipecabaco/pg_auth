-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000): listed-ids plan for small subjects, per-row check above.
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
select authz_internal.protect('app.comments', 'document', 'document_id', 'viewer');
select authz_internal.protect('app.attachments', 'comment', 'comment_id', 'viewer');
