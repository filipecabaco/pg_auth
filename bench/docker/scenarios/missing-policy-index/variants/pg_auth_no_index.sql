-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
-- pg_auth without the permission index: big subjects are checked with the engine (AuthzBatch).
-- Premise of this scenario: no index on org_id (an earlier variant may have created one).
drop index if exists app.documents_org_idx;
select authz.protect('app.documents', 'org', 'org_id', 'member');
