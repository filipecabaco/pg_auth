-- @set: pg_auth.sidecar_socket=/run/pg_auth/per_row.sock
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.orders', 'order', 'id', 'viewer');
select authz.protect('app.order_items', 'order', 'order_id', 'viewer');
select authz.protect('app.shipments', 'order', 'order_id', 'viewer');
