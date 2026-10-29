-- The order object: its buyer, and one item_seller edge per distinct seller among its items
-- (order_items -> products -> sellers, flattened). Seller staff are the seller's members.
select 'order:' || id, 'buyer', 'user:' || buyer_id from app.orders
union all
select distinct 'order:' || oi.order_id, 'item_seller', 'seller:' || p.seller_id
  from app.order_items oi join app.products p on p.id = oi.product_id
union all
select 'seller:' || seller_id, 'member', 'user:' || user_id from app.seller_members
