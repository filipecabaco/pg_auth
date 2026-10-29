-- The policy people write: EXISTS on the membership table (catalog issue 3 naive form).
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
create policy m on app.memberships for select to bench_user using (user_id = (select app.uid()));
create policy d on app.documents for select to bench_user
  using (exists (select 1 from app.memberships m where m.org_id = documents.org_id and m.user_id = (select app.uid())));
