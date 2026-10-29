-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- One binding covers select, insert, update and delete (read and write permission = member).
-- pg_auth installs separate select/insert/update/delete policies; there is no 'not deleted' filter.
select authz.protect('app.documents', 'org', 'org_id', 'member', 'member');
