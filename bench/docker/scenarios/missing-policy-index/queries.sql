-- name: count
select count(*) from app.documents

-- name: list
select id, title from app.documents order by id limit 50

-- name: pk
select * from app.documents where id = 4242
