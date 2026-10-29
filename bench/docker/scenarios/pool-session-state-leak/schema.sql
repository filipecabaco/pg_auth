create schema app;
create extension dblink;   -- used to hold ONE persistent "pooled server connection" per scenario call
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;
create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
insert into app.memberships select i, i from generate_series(1, 10) i;
create table app.docs(id int primary key, org_id int not null, title text not null);
insert into app.docs select i, 1 + (i % 10), 'doc ' || i from generate_series(1, :scale) i;
create index on app.docs(org_id);

-- Pool simulation. pool_open() opens a fresh dblink connection that stays open for the whole
-- scenario call and behaves like a pooler's server connection: every "request" below reuses it
-- and nothing is reset between requests (no DISCARD ALL / server_reset_query). The connection is
-- opened as superuser then drops to bench_user (the harness role is NOLOGIN).
create function app.pool_open() returns void language plpgsql security definer set search_path = pg_catalog, public as $f$
begin
  begin perform dblink_disconnect('pool'); exception when others then null; end;
  perform dblink_connect_u('pool', 'host=127.0.0.1 port=5432 dbname=bench user=postgres password=bench');
  perform dblink_exec('pool', 'set role bench_user');
end $f$;
create function app.pool_exec(sql text) returns void language sql as $f$ select public.dblink_exec('pool', sql) $f$;
create function app.pool_count() returns bigint language sql as
  $f$ select n from public.dblink('pool', 'select count(*) from app.docs') as t(n bigint) $f$;
create function app.pool_close() returns void language sql as $f$ select public.dblink_disconnect('pool') $f$;

-- Request A uses a session-level SET (the common ORM/pooler bug); request B is anonymous and sets
-- nothing. Returns what B can read.
create function app.session_set_then_anon() returns bigint language plpgsql as $f$
declare b bigint;
begin
  perform app.pool_open();
  perform app.pool_exec('set request.uid = ''1''');
  perform app.pool_count();              -- A's own request
  b := app.pool_count();                 -- B: same server connection, no identity
  perform app.pool_close();
  return b;
end $f$;

-- Same, but A does it correctly: SET LOCAL inside an explicit transaction. B is anonymous.
create function app.set_local_then_anon() returns bigint language plpgsql as $f$
declare b bigint;
begin
  perform app.pool_open();
  perform app.pool_exec('begin');
  perform app.pool_exec('set local request.uid = ''1''');
  perform app.pool_count();
  perform app.pool_exec('commit');
  b := app.pool_count();
  perform app.pool_close();
  return b;
end $f$;

-- The correct pattern must still work: rows seen by request A inside its transaction.
create function app.set_local_ok() returns bigint language plpgsql as $f$
declare a bigint;
begin
  perform app.pool_open();
  perform app.pool_exec('begin');
  perform app.pool_exec('set local request.uid = ''1''');
  a := app.pool_count();
  perform app.pool_exec('commit');
  perform app.pool_close();
  return a;
end $f$;

-- SET LOCAL outside a transaction is a no-op (warning). The request then runs with no identity.
create function app.local_outside_tx() returns bigint language plpgsql as $f$
declare a bigint;
begin
  perform app.pool_open();
  perform app.pool_exec('set local request.uid = ''1''');
  a := app.pool_count();
  perform app.pool_close();
  return a;
end $f$;

-- Brand-new server connection, identity never set.
create function app.fresh_anon() returns bigint language plpgsql as $f$
declare b bigint;
begin
  perform app.pool_open();
  b := app.pool_count();
  perform app.pool_close();
  return b;
end $f$;

-- Subject spoofing: a non-superuser overwrites the identity GUCs the trusted layer set (all of
-- request.uid and request.jwt.claims are plain user-settable GUCs).
-- Returns how many docs of org 2 are visible afterwards. The trusted subject is user 1 (org 1).
create function app.spoof(target text) returns bigint language plpgsql as $f$
declare n bigint;
begin
  perform set_config('request.uid', target, true);
  perform set_config('request.jwt.claims', json_build_object('sub', target)::text, true);
  select count(*) into n from app.docs where org_id = target::int;
  return n;
end $f$;
