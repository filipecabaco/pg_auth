-- Same array form on every verb (InitPlan array evaluated once per statement).
alter table app.documents enable row level security;
create policy s on app.documents for select to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy u on app.documents for update to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid())))) with check (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy d on app.documents for delete to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
