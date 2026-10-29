-- psql variable :scale = number of documents. Loaded once per container.
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
create index on app.documents(created_at);

-- helpers
create function app.is_member_plpgsql(o int) returns boolean language plpgsql stable as $f$
  begin return exists (select 1 from app.memberships where org_id = o and user_id = app.uid()); end $f$;
create function app.is_member_secdef(o int) returns boolean language sql stable security definer set search_path = '' as $f$
  select exists (select 1 from app.memberships where org_id = o and user_id = app.uid()) $f$;
create function app.my_orgs() returns setof int language sql stable security definer set search_path = '' as $f$
  select org_id from app.memberships where user_id = app.uid() $f$;
