-- Policy as written, supporting index missing.
drop index if exists app.documents_org_idx;
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user
  using (org_id = any (array(select app.my_orgs())));
