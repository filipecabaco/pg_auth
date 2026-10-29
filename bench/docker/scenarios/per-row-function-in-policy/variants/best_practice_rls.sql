-- Fix: turn the per-row function into a once-per-statement set (InitPlan array, indexable = ANY).
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user
  using (org_id = any (array(select app.my_orgs())));
