-- name: count
select count(*) from app.documents where owner_id = {{subject}}

-- name: recent
select id from app.documents where owner_id = {{subject}} order by created_at desc limit 20

-- name: pk
select * from app.documents where id = 1 and owner_id = {{subject}}
