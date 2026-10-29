-- Per-row PL/pgSQL membership function in the policy (the copy-paste shape).
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
create policy p on app.memberships for select to bench_user using (user_id = (select app.uid()));
create policy p on app.documents for select to bench_user using (app.is_member_plpgsql(tenant_id));
