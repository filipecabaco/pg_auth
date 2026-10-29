-- name: own_docs
select count(*) from app.docs where org_id = {{subject}}

-- name: own_memberships
select count(*) from app.memberships where user_id = {{subject}}

-- name: oracle_app_helper
select count(*) from app.memberships where user_id = {{subject}} and user_id <> {{subject}}
