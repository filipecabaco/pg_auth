-- IN (SELECT ...) form: hashed SubPlan, still not pulled up.
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
create policy p on app.memberships for select to bench_user using (user_id = app.uid());
create policy p on app.documents for select to bench_user
  using (org_id in (select m.org_id from app.memberships m where m.user_id = app.uid()));
