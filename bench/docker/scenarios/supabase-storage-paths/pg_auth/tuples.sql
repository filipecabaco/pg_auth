-- The folder keys acl_key holds: one object per user folder ('user-files_<uid>', owner), one per
-- team folder ('team-files_<team>', members from app.team_members), and the public bucket.
select distinct 'folder:user-files_' || owner, 'owner', 'user:' || owner
  from storage.objects where bucket_id = 'user-files'
union all
select 'folder:team-files_' || team_id, 'team_member', 'user:' || user_id from app.team_members
union all
select 'folder:public-assets', 'public', 'user:*'
