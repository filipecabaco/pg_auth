-- name: pk_small
select * from app.events where id = 1

-- name: pk_large
select * from app.events where id = 2

-- name: recent_page
select id, created_at from app.events order by created_at desc limit 20

-- name: count
select count(*) from app.events

-- name: join_count
select count(*), count(distinct p.id) from app.events e join app.projects p on p.id = e.project_id and p.tenant_id = e.tenant_id

-- name: join_page
select e.id, p.name from app.events e join app.projects p on p.id = e.project_id and p.tenant_id = e.tenant_id order by e.created_at desc limit 20

-- name: part_recent
select id, created_at from app.events_p order by created_at desc limit 20

-- name: part_count
select count(*) from app.events_p

-- name: notes_recent
select id, created_at from app.notes order by created_at desc limit 20

-- name: notes_count
select count(*) from app.notes
