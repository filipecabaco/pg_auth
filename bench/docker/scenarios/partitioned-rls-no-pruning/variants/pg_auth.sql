-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000): listed-ids plan for small subjects, per-row check above.
select authz.protect('app.documents_p16', 'org', 'org_id', 'member');
select authz.protect('app.documents_p128', 'org', 'org_id', 'member');
