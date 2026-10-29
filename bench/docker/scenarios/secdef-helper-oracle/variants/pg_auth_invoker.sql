-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- Like best_practice_rls: the helper runs as its caller, so memberships' own-row policy applies to it.
create or replace function app.is_member_of(p_user int, p_org int) returns boolean
  language sql stable security invoker set search_path = '' as
  $f$ select exists (select 1 from app.memberships where user_id = p_user and org_id = p_org) $f$;
alter function app.is_member_of(int, int) owner to postgres;
select authz.protect('app.docs', 'org', 'org_id', 'member');
select authz.protect('app.memberships', 'profile', 'user_id', 'owner');
