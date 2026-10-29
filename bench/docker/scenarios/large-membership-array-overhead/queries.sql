-- name: pk_a
select * from app.documents where id = 4

-- name: pk_b
select * from app.documents where id = 20

-- name: list
select id from app.documents order by id limit 20

-- name: count
select count(*) from app.documents
