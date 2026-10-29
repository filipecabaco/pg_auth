-- name: pk
select * from app.documents where id = 4242

-- name: count
select count(*) from app.documents

-- name: recent
select id from app.documents order by created_at desc limit 20

-- name: unindexed
select id from app.documents where title like '%7777%' limit 20

-- name: join
select count(*) from app.comments

-- name: part_count
select count(*) from app.documents_p

-- name: part_recent
select id from app.documents_p order by created_at desc limit 20
