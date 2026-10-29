-- name: l1_docs
select count(*) from app.documents

-- name: l2_comments
select count(*) from app.comments

-- name: l3_attachments
select count(*) from app.attachments

-- name: l3_pk
select * from app.attachments where id = 4242

-- name: l3_recent
select id from app.attachments order by id desc limit 20
