-- (select auth.uid()), the seller's order ids built once per query as an InitPlan array from a
-- security definer function, indexed policy columns, `to bench_user`.
alter table app.orders enable row level security;
alter table app.order_items enable row level security;
alter table app.shipments enable row level security;
create policy p on app.orders for select to bench_user
  using (buyer_id = (select auth.uid()) or id = any ((select app.seller_order_ids())::bigint[]));
create policy p on app.order_items for select to bench_user
  using (order_id = any ((select app.visible_order_ids())::bigint[]));
create policy p on app.shipments for select to bench_user
  using (order_id = any ((select app.visible_order_ids())::bigint[]));
