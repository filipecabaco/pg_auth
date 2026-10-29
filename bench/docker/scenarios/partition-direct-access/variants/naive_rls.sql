-- The policy people write: RLS + policy on the partitioned parent only (the leaves are left alone).
alter table app.documents_p enable row level security;
create policy p on app.documents_p for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
