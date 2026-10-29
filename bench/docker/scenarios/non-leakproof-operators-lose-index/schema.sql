-- psql variable :scale = number of documents.
select :scale::bigint as docs,
       greatest(:scale / 20, 10)::int as orgs,
       greatest(:scale / 20, 10)::int as users \gset

create extension if not exists pg_trgm;
create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
create index on app.memberships(org_id, user_id);
insert into app.memberships select u, 1 + (u::bigint * 7919) % :orgs from generate_series(1, :users) u;
insert into app.memberships select u, 1 + (u::bigint * 104729) % :orgs from generate_series(1, :users) u on conflict do nothing;
insert into app.memberships select 0, o from generate_series(1, :orgs, 2) o on conflict do nothing;

create table app.documents(id bigint primary key, org_id int not null,
                           title text not null, body text not null, meta jsonb not null, price numeric not null);
insert into app.documents
  select i, 1 + (i % :orgs),
         'Doc-' || (i % 5000),
         md5(i::text) || md5((i + 1)::text),
         jsonb_build_object('status', (array['draft','open','closed'])[1 + i % 3], 'n', i % 100),
         ((i % 1000) / 10.0)::numeric
  from generate_series(1, :docs) i;
create index on app.documents(org_id);
create index documents_body_trgm on app.documents using gin (body gin_trgm_ops);
create index documents_meta_gin on app.documents using gin (meta jsonb_path_ops);
create index documents_title_lower on app.documents (lower(title));
create index documents_title on app.documents (title);
create index documents_price on app.documents (price);

create function app.my_orgs() returns int[] language sql stable security definer as $f$
  select array(select org_id from app.memberships where user_id = app.uid()) $f$;
