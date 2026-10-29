-- name: join_agg
select d.title, count(*) from app.documents d join app.comments c on c.doc_id = d.id group by d.title order by 2 desc, 1 limit 10

-- name: join_count
select count(*) from app.documents d join app.comments c on c.doc_id = d.id

-- name: join_one_doc
select d.id, c.id from app.documents d join app.comments c on c.doc_id = d.id where d.id = 4242 order by c.id

-- name: join_filtered
select count(*) from app.documents d join app.comments c on c.doc_id = d.id where d.title = 'T7' and c.body like 'c1%'
