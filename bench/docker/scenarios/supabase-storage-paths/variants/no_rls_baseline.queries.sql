-- name: list_team_prefix
select name from storage.objects
where bucket_id = 'team-files' and name collate "C" >= '1/proj2/' and name collate "C" < '1/proj2/~' and (bucket_id = 'public-assets' or (bucket_id = 'user-files' and split_part(name, '/', 1) = '{{subject}}') or (bucket_id = 'team-files' and split_part(name, '/', 1)::int in (select team_id from app.team_members where user_id = '{{subject}}'::uuid)))
order by name collate "C" limit 100

-- name: list_own_prefix
select name from storage.objects
where bucket_id = 'user-files'
  and name collate "C" >= '{{subject}}/proj1/' and name collate "C" < '{{subject}}/proj1/~' and (bucket_id = 'public-assets' or (bucket_id = 'user-files' and split_part(name, '/', 1) = '{{subject}}') or (bucket_id = 'team-files' and split_part(name, '/', 1)::int in (select team_id from app.team_members where user_id = '{{subject}}'::uuid)))
order by name collate "C" limit 100

-- name: count_mine
select count(*) from storage.objects where owner = '{{subject}}'::uuid and (bucket_id = 'public-assets' or (bucket_id = 'user-files' and split_part(name, '/', 1) = '{{subject}}') or (bucket_id = 'team-files' and split_part(name, '/', 1)::int in (select team_id from app.team_members where user_id = '{{subject}}'::uuid)))

-- name: point_own
select id, size from storage.objects
where bucket_id = 'user-files' and name = '{{subject}}/proj0/avatar.png' and (bucket_id = 'public-assets' or (bucket_id = 'user-files' and split_part(name, '/', 1) = '{{subject}}') or (bucket_id = 'team-files' and split_part(name, '/', 1)::int in (select team_id from app.team_members where user_id = '{{subject}}'::uuid)))

-- name: point_public
select id, size from storage.objects where bucket_id = 'public-assets' and name = 'avatars/8.png' and (bucket_id = 'public-assets' or (bucket_id = 'user-files' and split_part(name, '/', 1) = '{{subject}}') or (bucket_id = 'team-files' and split_part(name, '/', 1)::int in (select team_id from app.team_members where user_id = '{{subject}}'::uuid)))

-- name: denied_folder
select count(*) from storage.objects
where bucket_id = 'user-files'
  and name collate "C" >= app.u(4)::text || '/' and name collate "C" < app.u(4)::text || '/~' and (bucket_id = 'public-assets' or (bucket_id = 'user-files' and split_part(name, '/', 1) = '{{subject}}') or (bucket_id = 'team-files' and split_part(name, '/', 1)::int in (select team_id from app.team_members where user_id = '{{subject}}'::uuid)))

-- name: recent
select name from storage.objects where (bucket_id = 'public-assets' or (bucket_id = 'user-files' and split_part(name, '/', 1) = '{{subject}}') or (bucket_id = 'team-files' and split_part(name, '/', 1)::int in (select team_id from app.team_members where user_id = '{{subject}}'::uuid))) order by created_at desc, id limit 20
