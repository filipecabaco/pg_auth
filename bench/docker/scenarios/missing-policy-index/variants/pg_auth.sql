-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the sidecar's default LIST_LIMIT (1000): listed-ids plan for small subjects, per-row check above.
-- Premise of this scenario: no index on org_id (an earlier variant may have created one).
drop index if exists app.documents_org_idx;
select authz_internal.protect('app.documents', 'org', 'org_id', 'member');
