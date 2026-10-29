-- name: count
select count(*) from app.documents

-- name: recent
select id from app.documents order by created_at desc limit 20

-- name: pk
select * from app.documents where id = 1
