-- name: via_parent
select count(*) from app.documents_p where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: leaf_2
select count(*) from app.documents_p_2 where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: leaf_11
select count(*) from app.documents_p_11 where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: leaf_ids
select id from app.documents_p_5 where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) order by id limit 20
