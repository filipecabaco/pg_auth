-- Same orders policy, but order_items and shipments defer to it with the common
-- "exists (select from orders)" pattern (nested RLS: the orders policy runs inside).
alter table app.orders enable row level security;
alter table app.order_items enable row level security;
alter table app.shipments enable row level security;
create policy p on app.orders for select to bench_user
  using (buyer_id = (select auth.uid()) or id = any ((select app.seller_order_ids())::bigint[]));
create policy p on app.order_items for select to bench_user
  using (exists (select 1 from app.orders o where o.id = order_items.order_id));
create policy p on app.shipments for select to bench_user
  using (exists (select 1 from app.orders o where o.id = shipments.order_id));
