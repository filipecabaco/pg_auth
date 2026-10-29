-- Catalog fix: closure table + array in an InitPlan (maintenance of app.folder_closure is the write-side cost).
alter table app.files enable row level security;
create policy closure on app.files for select to bench_user using (folder_id = any ((select app.my_folders())::int[]));
