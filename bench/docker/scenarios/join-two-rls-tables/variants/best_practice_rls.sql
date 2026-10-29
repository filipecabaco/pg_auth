-- Catalog fix: same indexable ANY(array) on both denormalised org_id columns.
alter table app.documents enable row level security;
alter table app.comments enable row level security;
create policy d on app.documents for select to bench_user using (org_id = any ((select app.my_orgs())::int[]));
create policy c on app.comments for select to bench_user using (org_id = any ((select app.my_orgs())::int[]));
