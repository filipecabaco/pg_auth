-- No custom claim: the tenant is looked up in app.members, as an array built once (InitPlan).
alter table app.events enable row level security;
alter table app.events_p enable row level security;
alter table app.projects enable row level security;
alter table app.notes enable row level security;
create policy p on app.events for select to bench_user
  using (tenant_id = any (array(select m.tenant_id from app.members m where m.user_id = (select auth.uid()))));
create policy p on app.events_p for select to bench_user
  using (tenant_id = any (array(select m.tenant_id from app.members m where m.user_id = (select auth.uid()))));
create policy p on app.projects for select to bench_user
  using (tenant_id = any (array(select m.tenant_id from app.members m where m.user_id = (select auth.uid()))));
create policy p on app.notes for select to bench_user using (owner_id = (select auth.uid()));
