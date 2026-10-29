-- Catalog fix for the pool problems: transaction-scoped identity (the queries use SET LOCAL in an
-- explicit transaction) and a policy that fails closed on unset / '' (NULL matches nothing).
-- It does NOT stop a role from overwriting the GUC (spoof_org2): no plain-RLS fix exists short of
-- signed claims or role-per-user.
alter table app.docs enable row level security;
create policy d on app.docs for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m
                             where m.user_id = nullif(current_setting('request.uid', true), '')::int)));
