-- name: insert_one
select app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 1) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) from i $q$)

-- name: insert_100
select app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 100) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) from i $q$)

-- name: insert_1000
select app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 1000) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) from i $q$)

-- name: insert_returning
select app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 100) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) || ' ' || sum(id) from i $q$)

-- name: insert_foreign
select app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 1) g, (select app.tenant_uuid(7) tid) t returning id) select 'inserted ' || count(*) from i $q$)

-- name: insert_draft
select app.attempt($q$ with i as (insert into app.drafts(id, tenant_id, title) select 1000000000 + g, (auth.jwt() ->> 'tenant_id')::uuid, 'draft ' || g from generate_series(1, 1) g returning id) select 'inserted ' || count(*) from i $q$)

-- name: update_one
with u as (update app.tasks set status = 'done' where id = (select min(id) from app.tasks) returning 1) select count(*) from u

-- name: update_100
with u as (update app.tasks set status = 'done' where id in (select id from app.tasks order by id limit 100) returning 1) select count(*) from u

-- name: update_1000
with u as (update app.tasks set status = 'done' where id in (select id from app.tasks order by id limit 1000) returning 1) select count(*) from u

-- name: update_own
with u as (update app.tasks set status = 'done' where owner_id = auth.uid() and status <> 'done' returning 1) select count(*) from u

-- name: update_join
with u as (update app.tasks t set status = 'archived' from app.projects p where p.id = t.project_id and p.archived and t.status = 'open' returning 1) select count(*) from u

-- name: update_move_foreign
select app.attempt($q$ with u as (update app.tasks set tenant_id = app.tenant_uuid(7) where id = (select min(id) from app.tasks) returning 1) select 'updated ' || count(*) from u $q$)

-- name: delete_one
with d as (delete from app.tasks where id = (select min(id) from app.tasks) returning 1) select count(*) from d

-- name: delete_100
with d as (delete from app.tasks where id in (select id from app.tasks order by id limit 100) returning 1) select count(*) from d

-- name: delete_1000
with d as (delete from app.tasks where id in (select id from app.tasks order by id limit 1000) returning 1) select count(*) from d
