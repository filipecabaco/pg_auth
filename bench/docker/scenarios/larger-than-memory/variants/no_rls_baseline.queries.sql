-- name: point
select id, val from app.big where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and id = 4242

-- name: range_page
select id, val from app.big where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and id >= 1000 order by id limit 100

-- name: tenant_page
select id, val from app.big where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and tenant_id = 1 order by id limit 100

-- name: full_count
select count(*) from app.big where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}})

-- name: heap_sum
select sum(val) from app.big where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}})
