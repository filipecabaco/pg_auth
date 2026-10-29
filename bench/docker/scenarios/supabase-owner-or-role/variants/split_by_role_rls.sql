-- What a careful author might try: two permissive policies (owner / role) instead of one OR.
-- Permissive policies are OR'd together by the planner into the same qual, so this changes nothing.
alter table app.todos enable row level security;
alter table app.todo_items enable row level security;
create policy owner on app.todos for select to bench_user using (owner_id = (select auth.uid()));
create policy staff on app.todos for select to bench_user
  using ((select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'));
create policy p on app.todo_items for select to bench_user
  using (exists (select 1 from app.todos t where t.id = todo_id));
