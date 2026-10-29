-- name: count
select count(*) from app.files where folder_id in (select c.descendant from app.folder_grants g join app.folder_closure c on c.ancestor = g.folder_id where g.user_id = {{subject}})

-- name: recent
select id from app.files where folder_id in (select c.descendant from app.folder_grants g join app.folder_closure c on c.ancestor = g.folder_id where g.user_id = {{subject}}) order by created_at desc limit 20

-- name: one_folder
select count(*) from app.files where folder_id = 400 and folder_id in (select c.descendant from app.folder_grants g join app.folder_closure c on c.ancestor = g.folder_id where g.user_id = {{subject}})

-- name: deep_folder
select count(*) from app.files where folder_id = 4000 and folder_id in (select c.descendant from app.folder_grants g join app.folder_closure c on c.ancestor = g.folder_id where g.user_id = {{subject}})
