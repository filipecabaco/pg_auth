-- name: buyer_page
select id, status, created_at from app.orders where buyer_id = auth.uid() order by created_at desc, id desc limit 20

-- name: seller_orders
select o.id, o.created_at, count(*) as items, sum(oi.qty * oi.unit_price) as total
from app.orders o
join app.order_items oi on oi.order_id = o.id
join app.products p on p.id = oi.product_id
where p.seller_id = (select sm.seller_id from app.seller_members sm where sm.user_id = auth.uid() order by 1 limit 1)
group by o.id, o.created_at order by o.created_at desc, o.id desc limit 20

-- name: seller_revenue
select o.status, count(distinct o.id) as orders, sum(oi.qty * oi.unit_price) as revenue
from app.order_items oi
join app.products p on p.id = oi.product_id
join app.orders o on o.id = oi.order_id
where p.seller_id = (select sm.seller_id from app.seller_members sm where sm.user_id = auth.uid() order by 1 limit 1)
group by o.status order by o.status

-- name: point_order
select o.id, o.status, oi.id as item_id, oi.product_id from app.orders o join app.order_items oi on oi.order_id = o.id
where o.id = 1 order by oi.id

-- name: shipments_open
select count(*) from app.shipments where status = 'in_transit'

-- name: count
select count(*) from app.orders
