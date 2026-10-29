-- Partial fix from the catalog: indexable ANY(array) so the small subject can drive from org_id.
alter table app.documents enable row level security;
create policy d on app.documents for select to bench_user using (org_id = any ((select app.my_orgs())::int[]));
