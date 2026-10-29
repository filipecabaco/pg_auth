-- Vulnerable pattern: RLS on the table, views created by the (non-FORCE) owner role.
alter table app.docs enable row level security;
create policy p on app.docs for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
alter view app.docs_v set (security_invoker = false);
alter view app.docs_vb set (security_invoker = false);
alter view app.docs_f set (security_invoker = false);
alter function app.docs_fn() security definer;
