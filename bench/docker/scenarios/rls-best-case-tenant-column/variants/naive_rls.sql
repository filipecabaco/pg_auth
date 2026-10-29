-- Supabase docs copy-paste: auth.jwt() and auth.uid() called directly in the policy. Policies name the role.
alter table app.events enable row level security;
alter table app.events_p enable row level security;
alter table app.projects enable row level security;
alter table app.notes enable row level security;
create policy p on app.events for select to bench_user using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid);
create policy p on app.events_p for select to bench_user using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid);
create policy p on app.projects for select to bench_user using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid);
create policy p on app.notes for select to bench_user using (owner_id = auth.uid());
