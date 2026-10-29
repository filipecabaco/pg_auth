create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create role app_owner nologin;                 -- owns the tables (migration role)
create role app_bypass nologin bypassrls;      -- e.g. a backup / replication role
create role bench_user_dummy nologin;
-- bench_user may SET ROLE to them, but does not inherit their privileges or ownership
grant app_owner to bench_user with inherit false, set true;
grant app_bypass to bench_user with inherit false, set true;
grant create, usage on schema app to app_owner;

create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
insert into app.memberships select i, i from generate_series(1, 10) i;

create table app.docs(id int primary key, org_id int not null, title text not null);
insert into app.docs select i, 1 + (i % 10), 'doc ' || i from generate_series(1, :scale) i;
alter table app.docs owner to app_owner;
alter table app.memberships owner to app_owner;

create table app.docs_p(id int not null, org_id int not null, title text not null, primary key (org_id, id)) partition by list (org_id);
create table app.docs_p1 partition of app.docs_p for values in (1, 2, 3, 4, 5);
insert into app.docs_p select * from app.docs where org_id <= 5;
alter table app.docs_p owner to app_owner;

create materialized view app.doc_counts as select org_id, count(*) as n from app.docs group by org_id;
alter materialized view app.doc_counts owner to app_owner;

create table app.refs(doc_id int references app.docs(id));

-- run q (a count(*) statement) as another role, then switch back
create function app.count_as(r text, q text) returns bigint language plpgsql as $f$
declare n bigint; me text := current_user;
begin
  execute format('set local role %I', r);
  execute q into n;
  execute format('set local role %I', me);
  return n;
end $f$;

-- Existence oracle through a foreign key: inserting a reference to doc i succeeds iff doc i
-- exists, even when the caller cannot see it (RI checks run with the owner's rights).
-- Returns the number of docs the caller cannot see whose existence was confirmed.
create function app.fk_probe(max_id int) returns bigint language plpgsql as $f$
declare n bigint := 0; i int;
begin
  for i in 1..max_id loop
    continue when exists (select 1 from app.docs where id = i);   -- visible: not a leak
    begin
      insert into app.refs values (i);
      raise exception 'exists' using errcode = 'P0001';
    exception
      when foreign_key_violation then null;
      when sqlstate 'P0001' then n := n + 1;
    end;
  end loop;
  return n;
end $f$;

grant usage on schema app to app_bypass;
grant select on app.docs to app_bypass;
