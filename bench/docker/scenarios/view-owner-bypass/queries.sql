-- Each query returns (rows visible, distinct orgs visible). Correct for user1: (scale/10, 1).
-- name: base_table
select count(*), count(distinct org_id) from app.docs

-- name: view_owner
select count(*), count(distinct org_id) from app.docs_v

-- name: view_invoker
select count(*), count(distinct org_id) from app.docs_vi

-- name: view_barrier
select count(*), count(distinct org_id) from app.docs_vb

-- name: view_on_view
select count(*), count(distinct org_id) from app.docs_vv

-- name: view_filtered
select count(*), count(distinct org_id) from app.docs_f where title like 'doc 1%'

-- name: secdef_function
select count(*), count(distinct org_id) from app.docs_fn()
