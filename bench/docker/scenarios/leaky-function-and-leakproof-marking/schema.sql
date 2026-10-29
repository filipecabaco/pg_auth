-- psql variable :scale = number of docs. 10 orgs; user N is a member of org N only.
create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.orgs(id int primary key, name text);
insert into app.orgs select i, 'org' || i from generate_series(1, 10) i;
create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
insert into app.memberships select i, i from generate_series(1, 10) i;

-- org 1 (the subject's org) has numeric secrets; every other org has non-numeric ones, so a cast
-- evaluated on a hidden row raises an error that carries the hidden value.
create table app.docs(id int primary key, org_id int not null, title text not null, secret text not null);
insert into app.docs select i, 1 + (i % 10), 'doc ' || i,
       case when 1 + (i % 10) = 1 then i::text else 'hidden-' || i end
  from generate_series(1, :scale) i;

-- The "spy": counts every invocation on a row the current subject (request.uid) is NOT a member
-- of, in a session GUC (rolled back with the transaction). The correct answer is always 0.
create function app.spy(org int) returns boolean language plpgsql volatile cost 0.0000001 as $f$
begin
  if org <> app.uid() then
    perform set_config('bench.leaked', (coalesce(nullif(current_setting('bench.leaked', true), ''), '0')::int + 1)::text, false);
  end if;
  return true;
end $f$;
create function app.spy_i(org int) returns int language plpgsql volatile cost 0.0000001 as $f$
begin
  perform app.spy(org);
  return org;
end $f$;
-- Reads the counter (volatile so it is evaluated after the scan that feeds it).
create function app.leaked() returns int language sql volatile as
  $f$ select coalesce(nullif(current_setting('bench.leaked', true), ''), '0')::int $f$;
-- Error channel: raises with the input value for non-numeric text.
create function app.to_int(t text) returns int language plpgsql immutable cost 0.0000001 as $f$
begin return t::int; end $f$;

create view app.docs_v with (security_invoker = true) as select * from app.docs;
