-- psql variable :scale = number of orders. Loaded once per container.
select greatest(:scale / 5, 100)::int as users,
       greatest(:scale / 300, 10)::int as sellers,
       greatest(:scale / 10, 100)::int as products \gset

create schema if not exists auth;
create or replace function auth.uid() returns uuid language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim.sub', true), ''),
                  (nullif(current_setting('request.jwt.claims', true), '')::jsonb ->> 'sub'))::uuid $$;
create or replace function auth.jwt() returns jsonb language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim', true), ''),
                  nullif(current_setting('request.jwt.claims', true), ''))::jsonb $$;
create or replace function auth.role() returns text language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim.role', true), ''),
                  (nullif(current_setting('request.jwt.claims', true), '')::jsonb ->> 'role'))::text $$;

create schema app;
create function app.u(n int) returns uuid language sql immutable as
  $$ select ('00000000-0000-0000-0000-' || lpad(n::text, 12, '0'))::uuid $$;

create table app.seller_members(seller_id int not null, user_id uuid not null, primary key (seller_id, user_id));
-- one staff user per seller (ids beyond the buyers), plus the named subjects
insert into app.seller_members select s, app.u(:users + s) from generate_series(1, :sellers) s;
insert into app.seller_members values (2, app.u(2)), (1, app.u(3)), (3, app.u(4));
create index on app.seller_members(user_id, seller_id);

create table app.products(id int primary key, seller_id int not null, title text not null, price numeric(10,2) not null);
-- seller 1 owns every 10th product; the rest are spread over all sellers
insert into app.products select p, case when p % 10 = 0 then 1 else 1 + p % :sellers end, 'product-' || p, 5 + p % 95
  from generate_series(1, :products) p;
create index on app.products(seller_id, id);

create table app.orders(id bigint primary key, buyer_id uuid not null, status text not null, created_at timestamptz not null);
insert into app.orders
  select i, app.u(case when i % 1000 = 1 then 1 when i % 500 = 3 then 4 else 5 + i % (:users - 4) end),
         (array['paid','packed','shipped','delivered'])[1 + i % 4], now() - (i || ' seconds')::interval
  from generate_series(1, :scale) i;
create index on app.orders(buyer_id, created_at desc, id desc);   -- buyer's order page
create index on app.orders(created_at desc, id desc);

create table app.order_items(id bigint primary key, order_id bigint not null, product_id int not null,
                             qty int not null, unit_price numeric(10,2) not null);
insert into app.order_items
  select j, 1 + (j - 1) % :scale, 1 + (j::bigint * 7919) % :products, 1 + j % 4, 5 + (1 + (j::bigint * 7919) % :products) % 95
  from generate_series(1, :scale * 3) j;
create index on app.order_items(order_id) include (product_id, qty, unit_price);   -- items of an order
create index on app.order_items(product_id) include (order_id, qty, unit_price);   -- seller side: product -> orders

create table app.shipments(id bigint primary key, order_id bigint not null, seller_id int not null,
                           status text not null, tracking text);
insert into app.shipments
  select oi.id, oi.order_id, p.seller_id, (array['label','in_transit','delivered'])[1 + oi.id % 3], md5(oi.id::text)
  from app.order_items oi join app.products p on p.id = oi.product_id
  where oi.id <= :scale and oi.id % 5 < 3;
create index on app.shipments(order_id);
create index on app.shipments(status, order_id);
analyze;

-- helpers for the RLS variants. security definer so the helper is not itself subject to the
-- policies it is called from (orders policy -> order_items would recurse: 42P17)
create function app.is_seller_of_order(oid bigint) returns boolean language plpgsql stable security definer as $f$
  begin
    return exists (select 1 from app.order_items oi
                   join app.products p on p.id = oi.product_id
                   join app.seller_members sm on sm.seller_id = p.seller_id
                   where oi.order_id = oid and sm.user_id = auth.uid());
  end $f$;
create function app.can_see_order(oid bigint) returns boolean language plpgsql stable security definer as $f$
  begin
    return exists (select 1 from app.orders o where o.id = oid and o.buyer_id = auth.uid())
        or app.is_seller_of_order(oid);
  end $f$;
create function app.seller_order_ids() returns bigint[] language sql stable security definer as $f$
  select array(select oi.order_id from app.seller_members sm
               join app.products p on p.seller_id = sm.seller_id
               join app.order_items oi on oi.product_id = p.id
               where sm.user_id = auth.uid()) $f$;
create function app.visible_order_ids() returns bigint[] language sql stable security definer as $f$
  select array(select id from app.orders where buyer_id = auth.uid()
               union select unnest(app.seller_order_ids())) $f$;
