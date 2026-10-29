-- Do not materialise the set: probe the grant table per candidate row (index on (user_id, doc_id)).
-- (The other catalog fix is grouping grants via groups.)
alter table app.documents enable row level security;
create policy p on app.documents for select to bench_user
  using (exists (select 1 from app.doc_grants g where g.doc_id = documents.id and g.user_id = (select app.uid())));
