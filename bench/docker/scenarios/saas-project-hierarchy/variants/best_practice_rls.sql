-- Supabase's performance advice: one security definer helper returning the visible project ids as an
-- array, evaluated once in an InitPlan ((select ...)), policies `to bench_user`, indexes on the
-- policy columns. comments and issue_labels only have issue_id, so they still probe issues per row.
alter table app.projects enable row level security;
create policy p on app.projects for select to bench_user using (id = any ((select app.my_projects())::int[]));
alter table app.issues enable row level security;
create policy p on app.issues for select to bench_user using (project_id = any ((select app.my_projects())::int[]));
alter table app.labels enable row level security;
create policy p on app.labels for select to bench_user using (project_id = any ((select app.my_projects())::int[]));
alter table app.comments enable row level security;
create policy p on app.comments for select to bench_user using (
  exists (select 1 from app.issues i where i.id = comments.issue_id));
alter table app.issue_labels enable row level security;
create policy p on app.issue_labels for select to bench_user using (
  exists (select 1 from app.issues i where i.id = issue_labels.issue_id));
