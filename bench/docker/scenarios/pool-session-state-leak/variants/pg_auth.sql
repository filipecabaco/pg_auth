-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- Identity lives in pg_auth.subject_id, a superuser-only GUC. The trusted layer (a pooler/connection
-- role granted SET ON PARAMETER, here: the superuser connection before SET ROLE) sets it; the request
-- role bench_user cannot. pa_* replay the same request sequences with that split.
select authz.protect('app.docs', 'org', 'org_id', 'member');

-- the pooled server connection: opened as superuser, pg_auth's socket set, then drops to bench_user
create function app.pa_open() returns void language plpgsql security definer set search_path = pg_catalog, public as $f$
begin
  begin perform dblink_disconnect('pool'); exception when others then null; end;
  perform dblink_connect_u('pool', 'host=127.0.0.1 port=5432 dbname=bench user=postgres password=bench');
  perform dblink_exec('pool', 'set pg_auth.sidecar_socket = ''/run/pg_auth/default.sock''');
  perform dblink_exec('pool', 'set role bench_user');
end $f$;
-- the trusted layer: leave bench_user, run sql (sets pg_auth.subject_id), return to bench_user
create function app.pa_trusted(sql text) returns void language plpgsql as $f$
begin
  perform public.dblink_exec('pool', 'reset role');
  perform public.dblink_exec('pool', sql);
  perform public.dblink_exec('pool', 'set role bench_user');
end $f$;

create function app.pa_session_set_then_anon() returns bigint language plpgsql as $f$
declare b bigint;
begin
  perform app.pa_open();
  perform app.pa_trusted('set pg_auth.subject_id = ''1''');   -- request A, session-level SET
  perform app.pool_count();
  b := app.pool_count();                                       -- request B: anonymous, same connection
  perform app.pool_close();
  return b;
end $f$;

create function app.pa_set_local_then_anon() returns bigint language plpgsql as $f$
declare b bigint;
begin
  perform app.pa_open();
  perform app.pool_exec('begin');
  perform app.pa_trusted('set local pg_auth.subject_id = ''1''');
  perform app.pool_count();
  perform app.pool_exec('commit');
  b := app.pool_count();
  perform app.pool_close();
  return b;
end $f$;

create function app.pa_set_local_ok() returns bigint language plpgsql as $f$
declare a bigint;
begin
  perform app.pa_open();
  perform app.pool_exec('begin');
  perform app.pa_trusted('set local pg_auth.subject_id = ''1''');
  a := app.pool_count();
  perform app.pool_exec('commit');
  perform app.pool_close();
  return a;
end $f$;

create function app.pa_local_outside_tx() returns bigint language plpgsql as $f$
declare a bigint;
begin
  perform app.pa_open();
  perform app.pa_trusted('set local pg_auth.subject_id = ''1''');   -- no transaction: no-op
  a := app.pool_count();
  perform app.pool_close();
  return a;
end $f$;

create function app.pa_fresh_anon() returns bigint language plpgsql as $f$
declare b bigint;
begin
  perform app.pa_open();
  b := app.pool_count();
  perform app.pool_close();
  return b;
end $f$;

-- Spoofing: the request role tries to become user 2 by setting the subject GUC. Not granted: 42501.
-- The statement keeps running as the trusted subject (user 1), so org 2's rows stay invisible.
create function app.pa_spoof(target text) returns bigint language plpgsql as $f$
declare n bigint;
begin
  begin
    perform set_config('pg_auth.subject_id', target, true);
    raise notice 'spoof succeeded';
  exception when insufficient_privilege then
    raise notice 'spoof denied: %', sqlerrm;
  end;
  perform set_config('request.uid', target, true);
  perform set_config('request.jwt.claims', json_build_object('sub', target)::text, true);
  select count(*) into n from app.docs where org_id = target::int;
  return n;
end $f$;
