-- The Supabase docs shape, copy-pasted: auth.uid() and auth.jwt() called bare, role test OR'd in.
-- Both are stable functions re-evaluated for every row (the jsonb parse of the claims included);
-- the child table asks the parent through nested RLS.
alter table app.todos enable row level security;
alter table app.todo_items enable row level security;
create policy p on app.todos for select to bench_user
  using (owner_id = auth.uid() or (auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'));
create policy p on app.todo_items for select to bench_user
  using (exists (select 1 from app.todos t where t.id = todo_id));
