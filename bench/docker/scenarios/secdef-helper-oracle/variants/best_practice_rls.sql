-- Catalog fix: the helper used by policies derives the subject itself (is_member); the
-- user-id-taking helper is recreated as SECURITY INVOKER so it is subject to the caller's RLS.
-- (REVOKE EXECUTE FROM PUBLIC is not shown: the harness re-grants EXECUTE on every function in
-- every non-system schema to bench_user after installing a variant.)
create or replace function app.is_member_of(p_user int, p_org int) returns boolean
  language sql stable security invoker set search_path = '' as
  $f$ select exists (select 1 from app.memberships where user_id = p_user and org_id = p_org) $f$;
alter table app.memberships enable row level security;
create policy m on app.memberships for select to bench_user using (user_id = (select app.uid()));
alter table app.docs enable row level security;
create policy d on app.docs for select to bench_user using (app.is_member(org_id));
