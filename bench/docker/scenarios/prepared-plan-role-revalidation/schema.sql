select :scale::bigint as docs,
       greatest(:scale / 100, 10)::int as orgs,
       greatest(:scale / 20, 10)::int as users \gset

create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
create index on app.memberships(org_id, user_id);
insert into app.memberships select u, 1 + (u::bigint * 7919) % :orgs from generate_series(1, :users) u;
insert into app.memberships select u, 1 + (u::bigint * 104729) % :orgs from generate_series(1, :users) u on conflict do nothing;
insert into app.memberships select 0, o from generate_series(1, :orgs, 2) o on conflict do nothing;

create table app.documents(id bigint primary key, org_id int not null, owner_id int not null,
                           title text not null, created_at timestamptz not null);
insert into app.documents
  select i, 1 + (i % :orgs), (i % :users) + 1, md5(i::text), now() - (i || ' seconds')::interval
  from generate_series(1, :docs) i;
create index on app.documents(org_id);

-- 64-way hash partitioned copy: planning cost grows with the partition count
create table app.documents_p(id bigint not null, org_id int not null, owner_id int not null,
                             title text not null, created_at timestamptz not null,
                             primary key (org_id, id)) partition by hash (org_id);
select format('create table app.documents_p_%s partition of app.documents_p for values with (modulus 64, remainder %s)', i, i)
  from generate_series(0, 63) i \gexec
insert into app.documents_p select * from app.documents;
create index on app.documents_p(id);

create function app.is_member_plpgsql(o int) returns boolean language plpgsql stable as $f$
  begin return exists (select 1 from app.memberships where org_id = o and user_id = app.uid()); end $f$;

-- Two interchangeable application roles (the PostgREST anon/authenticated pattern). The runner
-- connects as superuser and does `set local role bench_user`; SET ROLE inside the function is
-- checked against the *session* user (superuser), so no membership is needed.
do $$ begin
  create role bench_a nologin; create role bench_b nologin;
exception when duplicate_object then null; end $$;
grant usage on schema app to bench_a, bench_b;
grant select on all tables in schema app to bench_a, bench_b;

-- The harness can only run one statement per query and cannot PREPARE/EXECUTE across statements,
-- so the "prepared statement" is a PL/pgSQL one: the static SQL inside the loop is a cached plan
-- (custom plans for the first 5 runs, then possibly generic) that is re-validated whenever the
-- role changes (dependsOnRLS + rewriteRoleId in plancache.c). alt = alternate bench_a / bench_b on
-- every iteration; otherwise stay on bench_a (identical SET ROLE overhead in both cases).
select format($q$
create function app.loop_flat(n int, alt boolean) returns bigint language plpgsql as $f$
declare i int; c bigint; hits bigint := 0;
begin
  for i in 1..n loop
    execute case when alt and i %% 2 = 0 then 'set local role bench_b' else 'set local role bench_a' end;
    select count(*) into c from app.documents where id = (i::bigint * 7919) %% %1$s + 1;
    hits := hits + c;
  end loop;
  return hits;
end $f$ $q$, :docs) \gexec

select format($q$
create function app.loop_part(n int, alt boolean) returns bigint language plpgsql as $f$
declare i int; c bigint; hits bigint := 0;
begin
  for i in 1..n loop
    execute case when alt and i %% 2 = 0 then 'set local role bench_b' else 'set local role bench_a' end;
    select count(*) into c from app.documents_p where id = (i::bigint * 7919) %% %1$s + 1;
    hits := hits + c;
  end loop;
  return hits;
end $f$ $q$, :docs) \gexec
