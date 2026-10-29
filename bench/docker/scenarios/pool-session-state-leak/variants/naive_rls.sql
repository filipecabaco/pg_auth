-- Vulnerable pattern from the catalog: the policy casts the GUC directly. Fails open when a
-- session-level SET is inherited, crashes ('' or unknown GUC) when nothing / SET LOCAL residue.
alter table app.docs enable row level security;
create policy d on app.docs for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = current_setting('request.uid')::int)));
