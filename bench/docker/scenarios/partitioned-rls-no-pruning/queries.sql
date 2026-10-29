-- name: count16
select count(*) from app.documents_p16

-- name: count128
select count(*) from app.documents_p128

-- name: recent16
select id from app.documents_p16 order by created_at desc limit 20

-- name: recent128
select id from app.documents_p128 order by created_at desc limit 20

-- name: literal128
select count(*) from app.documents_p128 where org_id in (1, 3)
