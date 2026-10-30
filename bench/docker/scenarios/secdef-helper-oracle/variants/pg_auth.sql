-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- Same trap as naive_rls: the helper is SECURITY DEFINER, owned by the superuser that ran the schema.
-- pg_auth protects docs (org member) and memberships (own rows); the helper is app code and is not affected.
create or replace function app.is_member_of(p_user int, p_org int) returns boolean
  language sql stable security definer set search_path = '' as
  $f$ select exists (select 1 from app.memberships where user_id = p_user and org_id = p_org) $f$;
alter function app.is_member_of(int, int) owner to postgres;
select authz_internal.protect('app.docs', 'org', 'org_id', 'member');
select authz_internal.protect('app.memberships', 'profile', 'user_id', 'owner');
