-- name: list_team_prefix
select name from storage.objects
where bucket_id = 'team-files' and name collate "C" >= '1/proj2/' and name collate "C" < '1/proj2/~'
order by name collate "C" limit 100

-- name: list_own_prefix
select name from storage.objects
where bucket_id = 'user-files'
  and name collate "C" >= auth.uid()::text || '/proj1/' and name collate "C" < auth.uid()::text || '/proj1/~'
order by name collate "C" limit 100

-- name: count_mine
select count(*) from storage.objects where owner = auth.uid()

-- name: point_own
select id, size from storage.objects
where bucket_id = 'user-files' and name = auth.uid()::text || '/proj0/avatar.png'

-- name: point_public
select id, size from storage.objects where bucket_id = 'public-assets' and name = 'avatars/8.png'

-- name: denied_folder
select count(*) from storage.objects
where bucket_id = 'user-files'
  and name collate "C" >= app.u(4)::text || '/' and name collate "C" < app.u(4)::text || '/~'

-- name: recent
select name from storage.objects order by created_at desc, id limit 20
