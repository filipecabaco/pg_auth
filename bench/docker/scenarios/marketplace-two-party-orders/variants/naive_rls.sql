-- OR of buyer and seller in one policy; the seller branch is a security definer plpgsql helper
-- (the join through order_items cannot be written inline: 42P17). order_items and shipments
-- call a second helper per row.
alter table app.orders enable row level security;
alter table app.order_items enable row level security;
alter table app.shipments enable row level security;
create policy p on app.orders for select to bench_user using (buyer_id = auth.uid() or app.is_seller_of_order(id));
create policy p on app.order_items for select to bench_user using (app.can_see_order(order_id));
create policy p on app.shipments for select to bench_user using (app.can_see_order(order_id));
