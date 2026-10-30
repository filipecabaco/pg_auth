-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.orders', 'order', 'id', 'viewer');
select authz_internal.protect('app.order_items', 'order', 'order_id', 'viewer');
select authz_internal.protect('app.shipments', 'order', 'order_id', 'viewer');
