-- name: members
select count(*) from app.memberships

-- name: docs
select count(*) from app.docs

-- name: docs_join_members
select count(*) from app.docs d join app.memberships m on m.org_id = d.org_id

-- name: invites
select count(*) from app.invites
