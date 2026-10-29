-- The tenant -> user mapping the RLS variants use (subject id = user id), as OpenFGA tuples.
select 'tenant:' || tenant_id, 'member', 'user:' || user_id from app.tenant_users
