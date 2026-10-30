-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- The helper stays SECURITY DEFINER but is owned by an ordinary role (not a superuser, no BYPASSRLS).
-- pg_auth's policy is FORCEd and keyed on the request's subject, not on the executing role, so the
-- definer function still sees only the caller's own membership rows.
do $$ begin create role app_defn nologin; exception when duplicate_object then null; end $$;
grant usage on schema app to app_defn;
grant select on app.memberships to app_defn;
create or replace function app.is_member_of(p_user int, p_org int) returns boolean
  language sql stable security definer set search_path = '' as
  $f$ select exists (select 1 from app.memberships where user_id = p_user and org_id = p_org) $f$;
alter function app.is_member_of(int, int) owner to postgres;
alter function app.is_member_of(int, int) owner to app_defn;
select authz_internal.protect('app.docs', 'org', 'org_id', 'member');
select authz_internal.protect('app.memberships', 'profile', 'user_id', 'owner');
