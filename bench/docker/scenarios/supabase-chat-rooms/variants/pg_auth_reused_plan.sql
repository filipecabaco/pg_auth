-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- @set: plan_cache_mode=force_generic_plan
-- @prepare_as: two_rooms
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz.protect('app.rooms', 'room', 'id', 'viewer');
select authz.protect('app.room_members', 'room', 'room_id', 'viewer');
select authz.protect('app.messages', 'room', 'room_id', 'viewer');
select authz.protect('app.reactions', 'room', 'room_id', 'viewer');
select authz.protect('app.room_reads', 'user', 'user_id', 'owner');
