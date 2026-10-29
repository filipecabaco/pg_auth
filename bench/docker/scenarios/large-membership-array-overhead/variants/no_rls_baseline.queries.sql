-- name: pk_a
select * from app.documents d where id = 4 and exists (select 1 from app.doc_grants g where g.user_id = {{subject}} and g.doc_id = d.id)

-- name: pk_b
select * from app.documents d where id = 20 and exists (select 1 from app.doc_grants g where g.user_id = {{subject}} and g.doc_id = d.id)

-- name: list
select id from app.documents d where exists (select 1 from app.doc_grants g where g.user_id = {{subject}} and g.doc_id = d.id) order by id limit 20

-- name: count
select count(*) from app.documents d where exists (select 1 from app.doc_grants g where g.user_id = {{subject}} and g.doc_id = d.id)
