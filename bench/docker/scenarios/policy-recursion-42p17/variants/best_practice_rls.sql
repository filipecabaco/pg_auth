-- Catalog fix: SECURITY DEFINER helpers (owner = superuser, so RLS is bypassed; no user-id
-- argument, fixed search_path) break every cycle. Policies read helpers, never protected tables.
create or replace function app.my_orgs() returns int[] language sql stable security definer set search_path = '' as
  $f$ select array(select m.org_id from app.memberships m where m.user_id = (select current_setting('request.uid')::int)) $f$;
create or replace function app.my_invited() returns int[] language sql stable security definer set search_path = '' as
  $f$ select array(select i.doc_id from app.invites i where i.user_id = (select current_setting('request.uid')::int)) $f$;
create or replace function app.my_visible_docs() returns int[] language sql stable security definer set search_path = '' as
  $f$ select array(select d.id from app.docs d where d.org_id = any (app.my_orgs()) or d.id = any (app.my_invited())) $f$;
alter table app.memberships enable row level security;
create policy m on app.memberships for select to bench_user using (org_id = any ((select app.my_orgs())::int[]));
alter table app.docs enable row level security;
create policy d on app.docs for select to bench_user
  using (org_id = any ((select app.my_orgs())::int[]) or id = any ((select app.my_invited())::int[]));
alter table app.invites enable row level security;
create policy i on app.invites for select to bench_user using (doc_id = any ((select app.my_visible_docs())::int[]));
