-- name: recent
select * from app.documents order by created_at desc limit 20

-- name: recent_page
select * from app.documents order by created_at desc offset 100 limit 20

-- name: oldest
select * from app.documents order by created_at asc limit 20
