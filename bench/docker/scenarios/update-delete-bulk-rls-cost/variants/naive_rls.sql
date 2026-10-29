-- Catalog naive policy: function-based USING and WITH CHECK (plus SELECT and DELETE so statements work).
alter table app.documents enable row level security;
create policy s on app.documents for select to bench_user using (app.is_member_plpgsql(org_id));
create policy u on app.documents for update to bench_user using (app.is_member_plpgsql(org_id)) with check (app.is_member_plpgsql(org_id));
create policy d on app.documents for delete to bench_user using (app.is_member_plpgsql(org_id));
