-- Groups (with nesting), folder tree and shares, as tuples. A child group's members are members of
-- the parent via the userset group:<child>#member; OpenFGA resolves nesting and the folder subtree,
-- so no closure tables.
select 'group:' || group_id, 'member', 'user:' || user_id from app.group_users
union all select 'group:' || parent_id, 'member', 'group:' || child_id || '#member' from app.group_groups
union all select 'folder:' || id, 'parent', 'folder:' || parent_id from app.folders where parent_id is not null
union all select 'folder:' || folder_id, 'viewer', 'user:' || user_id from app.folder_shares where user_id is not null
union all select 'folder:' || folder_id, 'viewer', 'group:' || group_id || '#member' from app.folder_shares where group_id is not null
