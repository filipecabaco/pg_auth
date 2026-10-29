-- Supabase's advice: one policy per command, claims wrapped in (select ...) (an InitPlan evaluated
-- once per statement), policy column leads a btree index, policies `to bench_user`.
alter table app.tasks enable row level security;
alter table app.drafts enable row level security;
alter table app.projects enable row level security;
create policy s on app.tasks for select to bench_user using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid));
create policy i on app.tasks for insert to bench_user
  with check (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor');
create policy u on app.tasks for update to bench_user
  using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor')
  with check (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor');
create policy d on app.tasks for delete to bench_user
  using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor');
create policy s on app.drafts for select to bench_user using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid));
create policy i on app.drafts for insert to bench_user
  with check (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor');
create policy u on app.drafts for update to bench_user
  using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor')
  with check (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor');
create policy d on app.drafts for delete to bench_user
  using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid) and (select auth.jwt() ->> 'tenant_role') = 'editor');
create policy s on app.projects for select to bench_user using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid));
