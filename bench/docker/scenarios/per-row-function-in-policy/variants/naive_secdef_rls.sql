-- Same but the helper is a SQL SECURITY DEFINER function (also never inlined).
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user using (app.is_member_secdef(org_id));
