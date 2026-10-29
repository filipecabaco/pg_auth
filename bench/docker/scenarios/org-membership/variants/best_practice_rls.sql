-- Best shape plain RLS can reach: array built once as an InitPlan, indexable `= ANY`.
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
alter table app.documents_p enable row level security;
alter table app.comments enable row level security;
create policy p on app.memberships for select to bench_user using (user_id = (select app.uid()));
create policy p on app.documents for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy p on app.documents_p for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy p on app.comments for select to bench_user
  using (exists (select 1 from app.documents d where d.id = document_id));
