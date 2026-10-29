-- name: pk
select * from app.documents where id = 4242 and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: count
select count(*) from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: recent
select id from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc limit 20

-- name: unindexed
select id from app.documents where title like '%7777%' and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) limit 20

-- name: join
select count(*) from app.comments where document_id in (select id from app.documents where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}))

-- name: part_count
select count(*) from app.documents_p where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: part_recent
select id from app.documents_p where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc limit 20
