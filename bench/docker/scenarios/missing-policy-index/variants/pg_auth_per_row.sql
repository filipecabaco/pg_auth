-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
-- pg_auth with a sidecar that never lists: every row is checked.
-- Premise of this scenario: no index on org_id (an earlier variant may have created one).
drop index if exists app.documents_org_idx;
select authz.protect('app.documents', 'org', 'org_id', 'member');
