-- name: own_docs
select count(*) from app.docs

-- name: own_memberships
select count(*) from app.memberships

-- name: oracle_app_helper
-- how many OTHER users' (user, org) memberships can the caller discover through the helper? correct: 0
select count(*) from generate_series(1, 10) u, generate_series(1, 10) o where u <> app.uid() and app.is_member_of(u, o)
