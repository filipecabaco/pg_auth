-- Claims parsed once per statement: a helper turns app_metadata.orgs into the array of org ids
-- where the role grants the permission, evaluated as an InitPlan; `org_id = any (...)` is indexable.
-- Still claims: it sees the roles as of the last token refresh.
create function app.orgs_with(requested_permission text) returns int[] language sql stable as $$
  select coalesce(array_agg(j.key::int), '{}')
  from jsonb_each_text(auth.jwt() -> 'app_metadata' -> 'orgs') j
  join app.role_permissions rp on rp.role = j.value and rp.permission = requested_permission $$;
alter table app.channels enable row level security;
alter table app.messages enable row level security;
create policy p on app.channels for select to bench_user
  using (org_id = any ((select app.orgs_with('channels.read'))::int[]));
create policy p on app.messages for select to bench_user
  using (org_id = any ((select app.orgs_with('messages.read'))::int[]));
