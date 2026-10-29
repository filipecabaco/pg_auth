-- Catalog says there is no clean fix in plain RLS. Closest: an inlinable STABLE SQL function without the
-- (select ...) wrap. The planner folds stable expressions with the real value (MCV list is used, estimates are
-- right) at the price of re-evaluating current_setting per row where it is a plain filter (issues 1-2).
alter table app.events enable row level security;
create policy t on app.events for select to bench_user using (tenant_id = app.tenant());
