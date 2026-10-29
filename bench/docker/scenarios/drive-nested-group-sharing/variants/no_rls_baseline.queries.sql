-- name: point
select id, title from app.documents where id = 99 and folder_id in (select app.visible_folders_closure('{{subject}}'::uuid))

-- name: list_folder
select id, title, updated_at from app.documents where folder_id = 100 and folder_id in (select app.visible_folders_closure('{{subject}}'::uuid)) order by updated_at desc, id limit 50

-- name: subfolders
select f.id, f.name, count(d.id) n from app.folders f left join app.documents d on d.folder_id = f.id where f.parent_id = 2 and f.id in (select app.visible_folders_closure('{{subject}}'::uuid)) group by f.id, f.name order by f.id

-- name: count
select count(*) from app.documents where folder_id in (select app.visible_folders_closure('{{subject}}'::uuid))

-- name: tree_count
select count(*) from app.folders where id in (select app.visible_folders_closure('{{subject}}'::uuid))

-- name: recent
select id, title from app.documents where folder_id in (select app.visible_folders_closure('{{subject}}'::uuid)) order by updated_at desc, id limit 20

-- name: search
select id, title from app.documents where title ilike '%budget%' and folder_id in (select app.visible_folders_closure('{{subject}}'::uuid)) order by updated_at desc, id limit 20
