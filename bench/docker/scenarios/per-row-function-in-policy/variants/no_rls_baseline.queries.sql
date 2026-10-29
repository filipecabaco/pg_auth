-- name: count
select count(*) from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: pk
select * from app.documents where id = 4242 and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: recent
select id from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc limit 20

-- name: by_org
select count(*) from app.documents where org_id = 1 and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})
