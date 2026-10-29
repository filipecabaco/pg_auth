-- Supabase docs copy-paste: one SELECT policy and one FOR ALL write policy, auth.jwt() unwrapped
-- (called per row, including in WITH CHECK). The editor role comes from a custom claim.
alter table app.tasks enable row level security;
alter table app.drafts enable row level security;
alter table app.projects enable row level security;
create policy r on app.tasks for select to bench_user using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid);
create policy w on app.tasks for all to bench_user
  using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid and auth.jwt() ->> 'tenant_role' = 'editor')
  with check (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid and auth.jwt() ->> 'tenant_role' = 'editor');
create policy r on app.drafts for select to bench_user using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid);
create policy w on app.drafts for all to bench_user
  using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid and auth.jwt() ->> 'tenant_role' = 'editor')
  with check (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid and auth.jwt() ->> 'tenant_role' = 'editor');
create policy r on app.projects for select to bench_user using (tenant_id = (auth.jwt() ->> 'tenant_id')::uuid);
