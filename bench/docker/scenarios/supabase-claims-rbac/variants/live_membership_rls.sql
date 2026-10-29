-- The fresh alternative to claims: read org_members live in the policy (InitPlan array, indexable).
-- Always current, at the cost of a membership read per statement and no claim-sized token.
alter table app.channels enable row level security;
alter table app.messages enable row level security;
create policy p on app.channels for select to bench_user
  using (org_id = any (array(select m.org_id from app.org_members m
    join app.role_permissions rp on rp.role = m.role and rp.permission = 'channels.read'
    where m.user_id = (select auth.uid()))));
create policy p on app.messages for select to bench_user
  using (org_id = any (array(select m.org_id from app.org_members m
    join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read'
    where m.user_id = (select auth.uid()))));
