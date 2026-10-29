-- Indexable InitPlan array via SECURITY DEFINER helper. Still cannot help non-leakproof user quals.
alter table app.documents enable row level security;
create policy d on app.documents for select to bench_user using (org_id = any ((select app.my_orgs())::int[]));
