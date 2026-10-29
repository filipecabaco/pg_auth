create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;
-- Tables and views are owned by a non-superuser, NOBYPASSRLS owner (a typical migration role).
-- Table owners skip RLS unless FORCE is set, so views owned by it skip the policies too.
create role app_owner nologin;
grant create, usage on schema app to app_owner;

create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
insert into app.memberships select i, i from generate_series(1, 10) i;
create table app.docs(id int primary key, org_id int not null, title text not null);
insert into app.docs select i, 1 + (i % 10), 'doc ' || i from generate_series(1, :scale) i;
create index on app.docs(org_id);
alter table app.memberships owner to app_owner;
alter table app.docs owner to app_owner;

set role app_owner;
create view app.docs_v as select id, org_id, title from app.docs;                         -- owner bypasses RLS
create view app.docs_vi with (security_invoker = true) as select id, org_id, title from app.docs;
create view app.docs_vb with (security_barrier) as select id, org_id, title from app.docs; -- barrier != invoker
create view app.docs_vv with (security_invoker = true) as select * from app.docs_v;        -- invoker view over owner view
create view app.docs_f as select * from app.docs where org_id > 0;
reset role;
-- a SECURITY DEFINER set-returning function is the same bypass in function form
create function app.docs_fn() returns setof app.docs language sql stable security definer set search_path = pg_catalog, app as
  $f$ select * from app.docs $f$;
alter function app.docs_fn() owner to app_owner;
