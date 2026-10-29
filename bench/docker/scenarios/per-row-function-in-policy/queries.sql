-- name: count
select count(*) from app.documents

-- name: pk
select * from app.documents where id = 4242

-- name: recent
select id from app.documents order by created_at desc limit 20

-- name: by_org
select count(*) from app.documents where org_id = 1
