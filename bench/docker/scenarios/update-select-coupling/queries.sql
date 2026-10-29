-- name: update_where
with u as (update app.documents set title = title || '!' where org_id = (select min(org_id) from app.memberships where user_id = {{subject}}) returning 1) select count(*) from u

-- name: update_returning
with u as (update app.documents set title = title || '?' where org_id = (select min(org_id) from app.memberships where user_id = {{subject}}) returning id) select count(*), coalesce(sum(id), 0) from u

-- name: soft_delete
with u as (update app.documents set deleted = true where org_id = (select min(org_id) from app.memberships where user_id = {{subject}}) returning 1) select count(*) from u

-- name: delete_where
with d as (delete from app.documents where org_id = (select min(org_id) from app.memberships where user_id = {{subject}}) returning 1) select count(*) from d

-- name: insert_returning
with i as (insert into app.documents (id, org_id, owner_id, title, created_at) values (-1, (select min(org_id) from app.memberships where user_id = {{subject}}), 1, 't', now()) returning id) select count(*), coalesce(sum(id), 0) from i

-- name: upsert
with i as (insert into app.documents (id, org_id, owner_id, title, created_at) values ((select min(org_id) from app.memberships where user_id = {{subject}}) - 1 + (select orgs from app.cfg), (select min(org_id) from app.memberships where user_id = {{subject}}), 1, 'upserted', now()) on conflict (id) do update set title = excluded.title returning id) select count(*), coalesce(sum(id), 0) from i
