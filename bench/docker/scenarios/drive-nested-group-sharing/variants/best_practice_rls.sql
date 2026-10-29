-- Closure tables (folder_closure, group_ancestors) read through one security definer function,
-- evaluated once per statement as an InitPlan array. Reads are cheap; every folder move and every
-- group membership change must now rewrite closure rows.
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user using (folder_id = any ((select app.my_folders_closure())::int[]));
alter table app.folders enable row level security;
create policy p on app.folders for select to bench_user using (id = any ((select app.my_folders_closure())::int[]));
