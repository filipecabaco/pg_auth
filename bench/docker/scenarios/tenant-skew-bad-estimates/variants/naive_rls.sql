-- Catalog issue 17 policy: the (select ...) wrap makes the value an InitPlan param, invisible to statistics.
alter table app.events enable row level security;
create policy t on app.events for select to bench_user
  using (tenant_id = (select nullif(current_setting('request.uid', true), '')::int));
