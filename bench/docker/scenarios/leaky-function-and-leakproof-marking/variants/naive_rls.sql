-- Vulnerable pattern: plain RLS, but the superuser "fixed" a slow query by marking the user
-- functions LEAKPROOF. LEAKPROOF functions are allowed to run before the policy qual.
alter function app.spy(int) leakproof;
alter function app.spy_i(int) leakproof;
alter function app.to_int(text) leakproof;
alter table app.docs enable row level security;
create policy p on app.docs for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
