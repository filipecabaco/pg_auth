-- name: l1_docs
select count(*) from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: l2_comments
select count(*) from app.comments where document_id in (select d.id from app.documents d where d.org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}))

-- name: l3_attachments
select count(*) from app.attachments where comment_id in (select c.id from app.comments c where c.document_id in (select d.id from app.documents d where d.org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})))

-- name: l3_pk
select * from app.attachments where id = 4242 and comment_id in (select c.id from app.comments c where c.document_id in (select d.id from app.documents d where d.org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})))

-- name: l3_recent
select id from app.attachments where comment_id in (select c.id from app.comments c where c.document_id in (select d.id from app.documents d where d.org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}))) order by id desc limit 20
