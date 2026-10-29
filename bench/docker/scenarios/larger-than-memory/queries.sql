-- name: point
select id, val from app.big where id = 4242

-- name: range_page
select id, val from app.big where id >= 1000 order by id limit 100

-- name: tenant_page
select id, val from app.big where tenant_id = 1 order by id limit 100

-- name: full_count
select count(*) from app.big

-- name: heap_sum
select sum(val) from app.big
