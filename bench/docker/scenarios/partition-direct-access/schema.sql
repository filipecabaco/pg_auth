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

-- 16 hash partitions of documents_p. The runner grants bench_user DML on EVERY table of the schema,
-- leaves included: that is the "blanket GRANT ... ON ALL TABLES IN SCHEMA" the issue is about.
create table app.documents_p(id bigint not null, org_id int not null, owner_id int not null,
                             title text not null, created_at timestamptz not null,
                             primary key (org_id, id)) partition by hash (org_id);
select format('create table app.documents_p_%s partition of app.documents_p for values with (modulus 16, remainder %s)', i, i)
  from generate_series(0, 15) i \gexec
insert into app.documents_p
  select i, 1 + (i % :orgs), (i % :users) + 1, md5(i::text), now() - (i || ' seconds')::interval
  from generate_series(1, :docs) i;
create index on app.documents_p(created_at);
