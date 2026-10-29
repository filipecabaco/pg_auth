-- name: count16
select count(*) from app.documents_p16 where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: count128
select count(*) from app.documents_p128 where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: recent16
select id from app.documents_p16 where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc limit 20

-- name: recent128
select id from app.documents_p128 where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc limit 20

-- name: literal128
select count(*) from app.documents_p128 where org_id in (1, 3) and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})
