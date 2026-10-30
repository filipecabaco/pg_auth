-- Cedar's parents (child, parent): the tree, and each grant putting its folder in the user's scope
select 'Folder:' || id, 'Folder:' || parent_id from app.folders where parent_id is not null
union all
select 'Folder:' || folder_id, 'User:' || user_id from app.folder_grants
