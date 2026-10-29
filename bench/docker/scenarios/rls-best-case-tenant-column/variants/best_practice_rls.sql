-- Supabase's performance advice: the claim and auth.uid() are wrapped in (select ...) so they run once
-- per statement as an InitPlan; policy columns lead a btree index; policies are `to bench_user`.
alter table app.events enable row level security;
alter table app.events_p enable row level security;
alter table app.projects enable row level security;
alter table app.notes enable row level security;
create policy p on app.events for select to bench_user using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid));
create policy p on app.events_p for select to bench_user using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid));
create policy p on app.projects for select to bench_user using (tenant_id = (select (auth.jwt() ->> 'tenant_id')::uuid));
create policy p on app.notes for select to bench_user using (owner_id = (select auth.uid()));
