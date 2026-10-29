-- The folder tree and grants the RLS variants read, as OpenFGA tuples: viewing a folder is
-- inherited by its whole subtree, which OpenFGA resolves instead of a closure table. Grants are
-- viewer_direct, so pg_auth_hierarchy can ask for them alone.
select 'folder:' || id, 'parent', 'folder:' || parent_id from app.folders where parent_id is not null
union all
select 'folder:' || folder_id, 'viewer_direct', 'user:' || user_id from app.folder_grants
