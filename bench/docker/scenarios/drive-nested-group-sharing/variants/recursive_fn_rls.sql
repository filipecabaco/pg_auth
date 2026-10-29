-- The Supabase-advice shape without maintained closure tables: a security definer function expands
-- groups and the folder subtree with recursive CTEs, once per statement (InitPlan array). Reads of
-- one folder or one document pay for expanding everything the subject can see.
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user using (folder_id = any ((select app.my_folders_recursive())::int[]));
alter table app.folders enable row level security;
create policy p on app.folders for select to bench_user using (id = any ((select app.my_folders_recursive())::int[]));
