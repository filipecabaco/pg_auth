alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user
  using (id = any (array(select app.my_doc_ids())));
