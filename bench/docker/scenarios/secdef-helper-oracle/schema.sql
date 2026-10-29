create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;
create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
insert into app.memberships select i, i from generate_series(1, 10) i;
create table app.docs(id int primary key, org_id int not null, title text not null);
insert into app.docs select i, 1 + (i % 10), 'doc ' || i from generate_series(1, :scale) i;

-- The oracle from the catalog: owner rights (RLS skipped), subject taken from the argument.
create function app.is_member_of(p_user int, p_org int) returns boolean
  language sql stable security definer set search_path = '' as
  $f$ select exists (select 1 from app.memberships where user_id = p_user and org_id = p_org) $f$;
-- Fixed form: no user argument, subject derived from the session.
create function app.is_member(p_org int) returns boolean
  language sql stable security definer set search_path = '' as
  $f$ select exists (select 1 from app.memberships where user_id = app.uid() and org_id = p_org) $f$;
