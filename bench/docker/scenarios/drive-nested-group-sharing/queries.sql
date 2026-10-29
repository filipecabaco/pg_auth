-- name: point
select id, title from app.documents where id = 99

-- name: list_folder
select id, title, updated_at from app.documents where folder_id = 100 order by updated_at desc, id limit 50

-- name: subfolders
select f.id, f.name, count(d.id) n from app.folders f left join app.documents d on d.folder_id = f.id where f.parent_id = 2 group by f.id, f.name order by f.id

-- name: count
select count(*) from app.documents

-- name: tree_count
select count(*) from app.folders

-- name: recent
select id, title from app.documents order by updated_at desc, id limit 20

-- name: search
select id, title from app.documents where title ilike '%budget%' order by updated_at desc, id limit 20
