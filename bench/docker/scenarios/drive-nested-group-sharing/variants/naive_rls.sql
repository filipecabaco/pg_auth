-- How it is first written: per row, walk up the folder ancestors and look for a share to the user or to
-- any of the user's groups. A direct recursive CTE over app.folders inside the policy fails with
-- "infinite recursion detected in policy for relation folders" (folders has its own policy), so the
-- walk and the group expansion are security definer functions called from the policy.
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user using (exists (
  select 1 from app.folder_ancestors(documents.folder_id) a join app.folder_shares fs on fs.folder_id = a.a
  where fs.user_id = auth.uid() or fs.group_id in (select app.user_groups(auth.uid())))) ;
alter table app.folders enable row level security;
create policy p on app.folders for select to bench_user using (exists (
  select 1 from app.folder_ancestors(folders.id) a join app.folder_shares fs on fs.folder_id = a.a
  where fs.user_id = auth.uid() or fs.group_id in (select app.user_groups(auth.uid())))) ;
