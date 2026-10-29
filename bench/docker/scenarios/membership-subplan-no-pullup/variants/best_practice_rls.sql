-- Best plain RLS: indexable array built once as an InitPlan.
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user
  using (org_id = any (array(select app.my_orgs())));
