-- EXISTS on memberships on BOTH tables, each with its own org_id column (catalog issue 16 naive form).
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
alter table app.comments enable row level security;
create policy m on app.memberships for select to bench_user using (user_id = (select app.uid()));
create policy d on app.documents for select to bench_user
  using (exists (select 1 from app.memberships m where m.org_id = documents.org_id and m.user_id = (select app.uid())));
create policy c on app.comments for select to bench_user
  using (exists (select 1 from app.memberships m where m.org_id = comments.org_id and m.user_id = (select app.uid())));
