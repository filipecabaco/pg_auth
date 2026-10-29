-- The policy people write: PL/pgSQL helper taking the row's org_id.
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user using (app.is_member_plpgsql(org_id));
