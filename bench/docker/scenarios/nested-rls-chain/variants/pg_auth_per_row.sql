-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
-- pg_auth with a sidecar that never lists: every row is checked (one round trip per distinct id, cached for the transaction).
select authz.protect('app.documents', 'org', 'org_id', 'member');
select authz.protect('app.comments', 'document', 'document_id', 'viewer');
select authz.protect('app.attachments', 'comment', 'comment_id', 'viewer');
