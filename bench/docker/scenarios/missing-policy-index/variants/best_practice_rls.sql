-- Fix: index the policy column.
create index if not exists documents_org_idx on app.documents(org_id);
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user
  using (org_id = any (array(select app.my_orgs())));
