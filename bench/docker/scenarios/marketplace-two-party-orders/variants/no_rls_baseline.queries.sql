-- name: buyer_page
select id, status, created_at from app.orders o where buyer_id = '{{subject}}'::uuid and (o.buyer_id = '{{subject}}'::uuid or exists (select 1 from app.order_items x join app.products xp on xp.id = x.product_id join app.seller_members xs on xs.seller_id = xp.seller_id where x.order_id = o.id and xs.user_id = '{{subject}}'::uuid)) order by created_at desc, id desc limit 20

-- name: seller_orders
select o.id, o.created_at, count(*) as items, sum(oi.qty * oi.unit_price) as total
from app.orders o
join app.order_items oi on oi.order_id = o.id
join app.products p on p.id = oi.product_id
where p.seller_id = (select sm.seller_id from app.seller_members sm where sm.user_id = '{{subject}}'::uuid order by 1 limit 1)
  and (o.buyer_id = '{{subject}}'::uuid or exists (select 1 from app.order_items x join app.products xp on xp.id = x.product_id join app.seller_members xs on xs.seller_id = xp.seller_id where x.order_id = o.id and xs.user_id = '{{subject}}'::uuid))
group by o.id, o.created_at order by o.created_at desc, o.id desc limit 20

-- name: seller_revenue
select o.status, count(distinct o.id) as orders, sum(oi.qty * oi.unit_price) as revenue
from app.order_items oi
join app.products p on p.id = oi.product_id
join app.orders o on o.id = oi.order_id
where p.seller_id = (select sm.seller_id from app.seller_members sm where sm.user_id = '{{subject}}'::uuid order by 1 limit 1)
  and (o.buyer_id = '{{subject}}'::uuid or exists (select 1 from app.order_items x join app.products xp on xp.id = x.product_id join app.seller_members xs on xs.seller_id = xp.seller_id where x.order_id = o.id and xs.user_id = '{{subject}}'::uuid))
group by o.status order by o.status

-- name: point_order
select o.id, o.status, oi.id as item_id, oi.product_id from app.orders o join app.order_items oi on oi.order_id = o.id
where o.id = 1 and (o.buyer_id = '{{subject}}'::uuid or exists (select 1 from app.order_items x join app.products xp on xp.id = x.product_id join app.seller_members xs on xs.seller_id = xp.seller_id where x.order_id = o.id and xs.user_id = '{{subject}}'::uuid)) order by oi.id

-- name: shipments_open
select count(*) from app.shipments sh join app.orders o on o.id = sh.order_id where sh.status = 'in_transit' and (o.buyer_id = '{{subject}}'::uuid or exists (select 1 from app.order_items x join app.products xp on xp.id = x.product_id join app.seller_members xs on xs.seller_id = xp.seller_id where x.order_id = o.id and xs.user_id = '{{subject}}'::uuid))

-- name: count
select count(*) from app.orders o where (o.buyer_id = '{{subject}}'::uuid or exists (select 1 from app.order_items x join app.products xp on xp.id = x.product_id join app.seller_members xs on xs.seller_id = xp.seller_id where x.order_id = o.id and xs.user_id = '{{subject}}'::uuid))
