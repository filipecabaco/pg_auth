-- Correlated EXISTS on the membership table (which has its own RLS), as people write it.
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
create policy p on app.memberships for select to bench_user using (user_id = app.uid());
create policy p on app.documents for select to bench_user
  using (exists (select 1 from app.memberships m where m.org_id = documents.org_id and m.user_id = app.uid()));
