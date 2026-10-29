-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.orders', 'order', 'id', 'viewer');
select authz.protect('app.order_items', 'order', 'order_id', 'viewer');
select authz.protect('app.shipments', 'order', 'order_id', 'viewer');
