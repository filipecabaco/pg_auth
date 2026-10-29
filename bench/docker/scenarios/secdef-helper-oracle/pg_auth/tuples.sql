-- app.memberships as OpenFGA tuples: org membership (decides docs) and own-row visibility (decides memberships).
select 'org:' || org_id, 'member', 'user:' || user_id from app.memberships
union all
select 'profile:' || user_id, 'owner', 'user:' || user_id from app.memberships
