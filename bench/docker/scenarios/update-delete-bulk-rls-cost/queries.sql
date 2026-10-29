-- name: bulk_update
with u as (update app.documents set title = title || '!' where id % 2 = 0 returning 1) select count(*) from u

-- name: bulk_update_by_org
with u as (update app.documents set owner_id = owner_id where org_id % 4 = 0 returning 1) select count(*) from u

-- name: bulk_delete
with d as (delete from app.documents where id % 2 = 0 returning 1) select count(*) from d

-- name: bulk_update_returning
with u as (update app.documents set title = title || '!' where id % 2 = 0 returning id) select count(*), coalesce(sum(id), 0) from u
