-- Vulnerable pattern: the membership policy reads the membership table itself; docs reads
-- invites while invites reads docs (a cross-table cycle).
alter table app.memberships enable row level security;
create policy m on app.memberships for select to bench_user
  using (org_id in (select m2.org_id from app.memberships m2 where m2.user_id = app.uid()));
alter table app.docs enable row level security;
create policy d on app.docs for select to bench_user
  using (org_id in (select m.org_id from app.memberships m where m.user_id = app.uid())
         or id in (select i.doc_id from app.invites i where i.user_id = app.uid()));
alter table app.invites enable row level security;
create policy i on app.invites for select to bench_user
  using (doc_id in (select d.id from app.docs d));
