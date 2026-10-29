-- How the tutorials write it: every table re-derives access with its own EXISTS, comments ->
-- issues -> projects -> membership tables, unwrapped auth.uid(), no helper function.
alter table app.projects enable row level security;
create policy p on app.projects for select to bench_user using (
  exists (select 1 from app.org_admins a where a.org_id = projects.org_id and a.user_id = auth.uid())
  or exists (select 1 from app.team_members tm where tm.team_id = projects.team_id and tm.user_id = auth.uid())
  or exists (select 1 from app.project_team_shares s join app.team_members tm on tm.team_id = s.team_id
             where s.project_id = projects.id and tm.user_id = auth.uid())
  or exists (select 1 from app.project_guests g where g.project_id = projects.id and g.user_id = auth.uid()));
alter table app.issues enable row level security;
create policy p on app.issues for select to bench_user using (
  exists (select 1 from app.projects p where p.id = issues.project_id));
alter table app.comments enable row level security;
create policy p on app.comments for select to bench_user using (
  exists (select 1 from app.issues i where i.id = comments.issue_id));
alter table app.labels enable row level security;
create policy p on app.labels for select to bench_user using (
  exists (select 1 from app.projects p where p.id = labels.project_id));
alter table app.issue_labels enable row level security;
create policy p on app.issue_labels for select to bench_user using (
  exists (select 1 from app.issues i where i.id = issue_labels.issue_id));
