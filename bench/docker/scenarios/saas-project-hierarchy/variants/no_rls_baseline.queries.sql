-- name: point
select id, title, status from app.issues where id = 7 and project_id in (select app.visible_projects('{{subject}}'::uuid))

-- name: thread
select c.id, c.body from app.comments c join app.issues i on i.id = c.issue_id where i.id = 7 and i.project_id in (select app.visible_projects('{{subject}}'::uuid)) order by c.id

-- name: board
select status, count(*) from app.issues where project_id = 7 and project_id in (select app.visible_projects('{{subject}}'::uuid)) group by status order by status

-- name: my_open
select i.id, i.title, p.name from app.issues i join app.projects p on p.id = i.project_id where i.assignee_id = auth.uid() and i.status <> 'done' and p.id in (select app.visible_projects('{{subject}}'::uuid)) order by i.updated_at desc, i.id limit 20

-- name: project_stats
select p.id, count(*) total, count(*) filter (where i.status <> 'done') open_n from app.projects p join app.issues i on i.project_id = p.id where p.id in (select app.visible_projects('{{subject}}'::uuid)) group by p.id order by p.id

-- name: label_stats
select l.name, count(*) total, count(*) filter (where i.status <> 'done') open_n from app.issues i join app.projects p on p.id = i.project_id join app.issue_labels il on il.issue_id = i.id join app.labels l on l.id = il.label_id where p.id in (select app.visible_projects('{{subject}}'::uuid)) group by l.name order by l.name

-- name: comment_counts
select i.id, count(c.id) n from app.issues i left join app.comments c on c.issue_id = i.id where i.status = 'open' and i.project_id in (select app.visible_projects('{{subject}}'::uuid)) group by i.id order by n desc, i.id limit 20

-- name: busiest
select i.id, p.name, count(*) n from app.comments c join app.issues i on i.id = c.issue_id join app.projects p on p.id = i.project_id where p.id in (select app.visible_projects('{{subject}}'::uuid)) group by i.id, p.name order by n desc, i.id limit 20

-- name: search
select id, title from app.issues where title ilike '%crash%' and project_id in (select app.visible_projects('{{subject}}'::uuid)) order by updated_at desc, id limit 20

-- name: comment_search
select count(*) from app.comments where body ilike '%rollback%' and project_id in (select app.visible_projects('{{subject}}'::uuid))
