-- psql variable :scale = rows in documents and in comments.
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

create table app.documents(id bigint primary key, org_id int not null, title text not null);
insert into app.documents select i, 1 + (i % :orgs), 'T' || (i % 50) from generate_series(1, :docs) i;
create index on app.documents(org_id);

-- org_id is denormalised onto comments (the catalog's recommended shape for both tables)
create table app.comments(id bigint primary key, doc_id bigint not null, org_id int not null, body text);
insert into app.comments select i, d, 1 + (d % :orgs), 'c' || i
  from (select i, 1 + (i::bigint * 31) % :docs as d from generate_series(1, :docs) i) s;
create index on app.comments(doc_id);
create index on app.comments(org_id);

create function app.my_orgs() returns int[] language sql stable security definer as $f$
  select array(select org_id from app.memberships where user_id = app.uid()) $f$;
