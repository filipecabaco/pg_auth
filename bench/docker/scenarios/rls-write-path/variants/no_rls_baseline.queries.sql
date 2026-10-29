-- name: insert_one
select case when exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.role = 'editor') then app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 1) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) from i $q$) else 'denied' end

-- name: insert_100
select case when exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.role = 'editor') then app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 100) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) from i $q$) else 'denied' end

-- name: insert_1000
select case when exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.role = 'editor') then app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 1000) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) from i $q$) else 'denied' end

-- name: insert_returning
select case when exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.role = 'editor') then app.attempt($q$ with i as (insert into app.tasks(id, tenant_id, project_id, owner_id, title, status, created_at) select 1000000000 + g, t.tid, (select p.id from app.projects p where p.tenant_id = t.tid limit 1), auth.uid(), 'new ' || g, 'open', now() from generate_series(1, 100) g, (select (auth.jwt() ->> 'tenant_id')::uuid tid) t returning id) select 'inserted ' || count(*) || ' ' || sum(id) from i $q$) else 'denied' end

-- name: insert_foreign
select 'denied'

-- name: insert_draft
select case when exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.role = 'editor') then app.attempt($q$ with i as (insert into app.drafts(id, tenant_id, title) select 1000000000 + g, (auth.jwt() ->> 'tenant_id')::uuid, 'draft ' || g from generate_series(1, 1) g returning id) select 'inserted ' || count(*) from i $q$) else 'denied' end

-- name: update_one
with u as (update app.tasks t set status = 'done' where t.id = (select min(s.id) from app.tasks s where s.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}')) and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') returning 1) select count(*) from u

-- name: update_100
with u as (update app.tasks t set status = 'done' where t.id in (select s.id from app.tasks s where s.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') order by s.id limit 100) and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') returning 1) select count(*) from u

-- name: update_1000
with u as (update app.tasks t set status = 'done' where t.id in (select s.id from app.tasks s where s.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') order by s.id limit 1000) and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') returning 1) select count(*) from u

-- name: update_own
with u as (update app.tasks t set status = 'done' where t.owner_id = '{{subject}}'::uuid and t.status <> 'done' and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') returning 1) select count(*) from u

-- name: update_join
with u as (update app.tasks t set status = 'archived' from app.projects p where p.id = t.project_id and p.archived and t.status = 'open' and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') and p.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') returning 1) select count(*) from u

-- name: update_move_foreign
select case when exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.role = 'editor') then 'denied' else 'updated 0' end

-- name: delete_one
with d as (delete from app.tasks t where t.id = (select min(s.id) from app.tasks s where s.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}')) and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') returning 1) select count(*) from d

-- name: delete_100
with d as (delete from app.tasks t where t.id in (select s.id from app.tasks s where s.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') order by s.id limit 100) and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') returning 1) select count(*) from d

-- name: delete_1000
with d as (delete from app.tasks t where t.id in (select s.id from app.tasks s where s.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') order by s.id limit 1000) and t.tenant_id in (select m.tenant_id from app.members m where m.user_id = '{{subject}}') and exists (select 1 from app.members m where m.user_id = '{{subject}}' and m.tenant_id = t.tenant_id and m.role = 'editor') returning 1) select count(*) from d
