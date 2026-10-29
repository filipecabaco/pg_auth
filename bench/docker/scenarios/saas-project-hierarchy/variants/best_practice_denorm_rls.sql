-- best_practice_rls plus the usual last fix: project_id copied onto comments and issue_labels so
-- every policy is the same `project_id = any (InitPlan array)`. The price is a column per child table
-- that every write path must keep equal to its issue's project (and moving an issue must rewrite it).
alter table app.projects enable row level security;
create policy p on app.projects for select to bench_user using (id = any ((select app.my_projects())::int[]));
alter table app.issues enable row level security;
create policy p on app.issues for select to bench_user using (project_id = any ((select app.my_projects())::int[]));
alter table app.labels enable row level security;
create policy p on app.labels for select to bench_user using (project_id = any ((select app.my_projects())::int[]));
alter table app.comments enable row level security;
create policy p on app.comments for select to bench_user using (project_id = any ((select app.my_projects())::int[]));
alter table app.issue_labels enable row level security;
create policy p on app.issue_labels for select to bench_user using (project_id = any ((select app.my_projects())::int[]));
