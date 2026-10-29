-- name: app_role
select count(*) from app.docs where org_id = {{subject}}

-- name: owner_role
select count(*) from app.docs where org_id = {{subject}}

-- name: bypassrls_role
select count(*) from app.docs where org_id = {{subject}}

-- name: matview
select coalesce(sum(n), 0) from app.doc_counts where org_id = {{subject}}

-- name: part_parent
select count(*) from app.docs_p where org_id = {{subject}}

-- name: part_leaf_direct
select count(*) from app.docs_p1 where org_id = {{subject}}

-- name: part_late_leaf
select count(*) from app.docs_p_late where org_id = {{subject}}

-- name: fk_existence_oracle
select 0::bigint
