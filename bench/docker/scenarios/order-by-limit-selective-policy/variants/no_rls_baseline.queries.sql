-- name: recent
select * from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc limit 20

-- name: recent_page
select * from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc offset 100 limit 20

-- name: oldest
select * from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at asc limit 20
