-- name: count
select count(*) from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: list
select id, title from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by id limit 50

-- name: pk
select * from app.documents where id = 4242 and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})
