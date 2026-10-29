-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: pg_auth.permission_index=off
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.rooms', 'room', 'id', 'viewer');
select authz.protect('app.room_members', 'room', 'room_id', 'viewer');
select authz.protect('app.messages', 'room', 'room_id', 'viewer');
select authz.protect('app.reactions', 'room', 'room_id', 'viewer');
select authz.protect('app.room_reads', 'user', 'user_id', 'owner');
