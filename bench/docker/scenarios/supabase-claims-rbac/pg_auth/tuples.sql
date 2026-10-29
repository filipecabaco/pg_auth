-- One tuple per membership: the same rows the JWT claims are minted from.
select 'org:' || org_id, role, 'user:' || user_id from app.org_members
