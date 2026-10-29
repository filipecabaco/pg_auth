-- Rows visible to user1 through each path. Correct: only org 1 (scale/10; scale/20 in docs_p).
-- name: app_role
select count(*) from app.docs

-- name: owner_role
select app.count_as('app_owner', 'select count(*) from app.docs')

-- name: bypassrls_role
select app.count_as('app_bypass', 'select count(*) from app.docs')

-- name: matview
select coalesce(sum(n), 0) from app.doc_counts

-- name: part_parent
select count(*) from app.docs_p

-- name: part_leaf_direct
select count(*) from app.docs_p1

-- name: part_late_leaf
select count(*) from app.docs_p_late

-- name: fk_existence_oracle
select app.fk_probe(100)
