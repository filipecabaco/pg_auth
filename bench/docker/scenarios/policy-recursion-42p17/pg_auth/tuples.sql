-- app.memberships, app.docs.org_id and app.invites as OpenFGA tuples.
select 'org:' || org_id, 'member', 'user:' || user_id from app.memberships
union all
select 'doc:' || id, 'org', 'org:' || org_id from app.docs
union all
select 'doc:' || doc_id, 'invitee', 'user:' || user_id from app.invites
