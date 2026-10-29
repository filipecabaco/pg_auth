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

create table app.comments(id bigint primary key, document_id bigint not null, body text);
insert into app.comments select i, 1 + (i::bigint * 31) % :docs, 'c' || i from generate_series(1, :docs) i;
create index on app.comments(document_id);

create table app.documents_p(id bigint not null, org_id int not null, owner_id int not null,
                             title text not null, created_at timestamptz not null,
                             primary key (org_id, id)) partition by hash (org_id);
select format('create table app.documents_p_%s partition of app.documents_p for values with (modulus 16, remainder %s)', i, i)
  from generate_series(0, 15) i \gexec
insert into app.documents_p select * from app.documents;
create index on app.documents_p(created_at);

-- helper functions used by the naive / best-practice variants
create function app.is_member_plpgsql(o int) returns boolean language plpgsql stable as $f$
  begin return exists (select 1 from app.memberships where org_id = o and user_id = app.uid()); end $f$;
create function app.my_orgs() returns int[] language sql stable security definer as $f$
  select array(select org_id from app.memberships where user_id = app.uid()) $f$;
