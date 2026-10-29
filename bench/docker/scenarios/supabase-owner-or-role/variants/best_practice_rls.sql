-- Supabase's performance advice applied: (select auth.uid()) / (select auth.jwt() ...) are
-- evaluated once per statement as InitPlans, policies are `to bench_user`, policy columns indexed.
-- The OR with a row-independent InitPlan still cannot use the owner_id index.
alter table app.todos enable row level security;
alter table app.todo_items enable row level security;
create policy p on app.todos for select to bench_user
  using (owner_id = (select auth.uid())
         or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'));
create policy p on app.todo_items for select to bench_user
  using (exists (select 1 from app.todos t where t.id = todo_id));
