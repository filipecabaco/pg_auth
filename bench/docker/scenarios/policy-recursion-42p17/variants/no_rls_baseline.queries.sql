-- name: members
select count(*) from app.memberships where org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: docs
select count(*) from app.docs where (org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) or id in (select doc_id from app.invites where user_id = {{subject}}))

-- name: docs_join_members
select count(*) from app.docs d join app.memberships m on m.org_id = d.org_id
 where (d.org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) or d.id in (select doc_id from app.invites where user_id = {{subject}})) and m.org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: invites
select count(*) from app.invites where doc_id in (select id from app.docs where (org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}}) or id in (select doc_id from app.invites where user_id = {{subject}})))
