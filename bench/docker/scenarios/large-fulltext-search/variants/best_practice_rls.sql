-- Best shape plain RLS can reach: tenant array built once as an InitPlan, indexable `= ANY`.
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
create policy p on app.memberships for select to bench_user using (user_id = (select app.uid()));
create policy p on app.documents for select to bench_user
  using (tenant_id = any (array(select m.tenant_id from app.memberships m where m.user_id = (select app.uid()))));
