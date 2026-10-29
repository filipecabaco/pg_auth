-- name: count
select count(*) from app.files

-- name: recent
select id from app.files order by created_at desc limit 20

-- name: one_folder
select count(*) from app.files where folder_id = 400

-- name: deep_folder
select count(*) from app.files where folder_id = 4000
