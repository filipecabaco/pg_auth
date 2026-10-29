-- Supabase's RBAC guide shape, with the org carried in the claims: authorize(permission, org_id)
-- is a plpgsql security definer function called for every row. Each call parses
-- auth.jwt() -> 'app_metadata' -> 'orgs' and probes role_permissions.
create function app.authorize(requested_permission text, org int) returns boolean
language plpgsql stable security definer set search_path = '' as $$
declare
  user_role text;
  bind_permissions int;
begin
  select auth.jwt() -> 'app_metadata' -> 'orgs' ->> org::text into user_role;
  select count(*) into bind_permissions from app.role_permissions
   where role_permissions.permission = requested_permission and role_permissions.role = user_role;
  return bind_permissions > 0;
end;
$$;
alter table app.channels enable row level security;
alter table app.messages enable row level security;
create policy p on app.channels for select to bench_user using (app.authorize('channels.read', org_id));
create policy p on app.messages for select to bench_user using (app.authorize('messages.read', org_id));
