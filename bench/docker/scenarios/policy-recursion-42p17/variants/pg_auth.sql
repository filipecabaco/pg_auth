-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- No policy reads another table, so there is nothing to recurse into: the engine answers each question.
-- memberships by org (members see their orgs' members), docs by their own id (org member or invitee),
-- invites by the doc they belong to (visible when the doc is).
select authz_internal.protect('app.memberships', 'org', 'org_id', 'member');
select authz_internal.protect('app.docs', 'doc', 'id', 'viewer');
select authz_internal.protect('app.invites', 'doc', 'doc_id', 'viewer');
