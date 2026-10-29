-- The same memberships the RLS variants read from app.memberships, as OpenFGA tuples.
select 'org:' || org_id, 'member', 'user:' || user_id from app.memberships
union all
select 'document:' || id, 'org', 'org:' || org_id from app.documents
