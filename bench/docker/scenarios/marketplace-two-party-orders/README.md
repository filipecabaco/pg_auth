# marketplace-two-party-orders

An order is visible to its buyer OR to staff of any seller whose product is in it.

## The pattern

`orders(buyer_id)`, `order_items(order_id, product_id)`, `products(seller_id)`,
`seller_members(seller_id, user_id)`, `shipments(order_id)`. `order_items` and `shipments` inherit
the order's visibility. Queries: `buyer_page` (buyer's latest 20), `seller_orders` (seller's latest
20 with item count and total: 3-way join + group by), `seller_revenue` (join + aggregate by status),
`point_order` (order with its items), `shipments_open` (count), `count`.
Subjects: `buyer`, `small_seller`, `big_seller` (in ~27% of orders), `buyer_and_seller` (both roles).

## Why RLS is good / bad here

- Good: the buyer branch is a plain indexed column; a buyer sees the same speed as no RLS.
- Bad: the seller branch is a 3-table derivation inside an OR. Written inline it hits 42P17
  (`orders` policy -> `order_items` policy -> `orders`), so it moves into security definer helpers:
  per row (`naive_rls`) every order page rescans items, products and staff; as an InitPlan array
  (`best_practice_rls`) the whole set of the seller's order ids is built on every query, however
  small the page. `best_practice_nested_rls` keeps that orders policy and lets items/shipments use
  `exists (select from orders)`.

## What pg_auth does

Models the order once: `buyer: [user]`, `item_seller: [seller]`, `viewer: buyer or member from
item_seller`; `orders.id`, `order_items.order_id`, `shipments.order_id` are protected by it. The
"seller of any item" derivation moves into the tuples (one `item_seller` edge per distinct
(order, seller)).
Limitation / cost: tuples grow with orders (one buyer tuple per order, about 2.7 item_seller tuples
per order), unlike membership-style scenarios; a new order item must add its tuple (a write path
concern RLS does not have). Seller-visible columns per item (seller sees only its own items) cannot be
expressed: pg_auth protects whole rows by one object.

## Indexes and what each query should use

- `buyer_page`: `orders(buyer_id, created_at desc, id desc)`. `seller_orders` / `seller_revenue`:
  `products(seller_id, id)` -> `order_items(product_id) include (order_id, qty, unit_price)` (covering) ->
  `orders` pk. `point_order`: pk + `order_items(order_id) include (...)`. `shipments_open`:
  `shipments(status, order_id)`. Policy columns: `seller_members(user_id, seller_id)`,
  `shipments(order_id)`.

## Scale

`:scale` = orders (3 items each, shipments on ~60%); 10x = 300k orders, 900k items, ~1.1M tuples.
generate_series inserts only.

## Sources

- Supabase RLS guide (policies with joins, performance recommendations):
  https://supabase.com/docs/guides/database/postgres/row-level-security
- Supabase "RLS performance and best practices":
  https://supabase.com/docs/guides/troubleshooting/rls-performance-and-best-practices-Z5Jjwv
- Postgres policy recursion (42P17): https://www.postgresql.org/docs/current/sql-createpolicy.html

## Results (scale 30000, 3 reps, 2 CPU / 2 GB, median ms, shared host so indicative)

| subject / query | baseline | naive_rls | best_practice_rls | pg_auth | pg_auth_no_index |
|---|---|---|---|---|---|
| buyer / buyer_page | 0.020 | 133 | 0.045 | 0.087 | 0.066 |
| big_seller / seller_orders | 160 | 289 | 73 | 8.8 | 80 |
| big_seller / shipments_open | 168 | 126 | 1.6 | 0.44 | 5.3 |
| big_seller / buyer_page | 0.009 | 517 | 2.8 | 2.5 | 2277 |
| small_seller / seller_orders | 3.6 | 17.7 | 1.8 | 9.6 | 10.2 |

pg_auth wins for the big seller (the permission index answers the OR without per-query array
building), loses to best-practice RLS for the small seller's `seller_orders` (about 5x), and
`pg_auth_no_index` is badly slow for broad subjects (engine checks per row). The explicit baseline
(correlated EXISTS) is itself slow for big sellers. All variants returned identical results.
