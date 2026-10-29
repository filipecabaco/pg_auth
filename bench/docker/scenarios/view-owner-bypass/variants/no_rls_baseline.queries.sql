-- Correct answer: only the subject's own rows are visible through every access path.

-- name: base_table
select count(*), count(distinct org_id) from app.docs where org_id = {{subject}}

-- name: view_owner
select count(*), count(distinct org_id) from app.docs where org_id = {{subject}}

-- name: view_invoker
select count(*), count(distinct org_id) from app.docs where org_id = {{subject}}

-- name: view_barrier
select count(*), count(distinct org_id) from app.docs where org_id = {{subject}}

-- name: view_on_view
select count(*), count(distinct org_id) from app.docs where org_id = {{subject}}

-- name: secdef_function
select count(*), count(distinct org_id) from app.docs where org_id = {{subject}}

-- name: view_filtered
select count(*), count(distinct org_id) from app.docs where org_id = {{subject}} and title like 'doc 1%'
