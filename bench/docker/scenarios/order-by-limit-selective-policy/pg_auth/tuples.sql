-- The memberships the RLS variants read, as OpenFGA tuples.
select 'org:' || org_id, 'member', 'user:' || user_id from app.memberships
