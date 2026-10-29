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

-- the same data in two hash-partitioned tables: 16 and 128 partitions (partition-count sweep)
create table app.documents_p16(id bigint not null, org_id int not null, owner_id int not null,
                               title text not null, created_at timestamptz not null,
                               primary key (org_id, id)) partition by hash (org_id);
select format('create table app.documents_p16_%s partition of app.documents_p16 for values with (modulus 16, remainder %s)', i, i)
  from generate_series(0, 15) i \gexec
create table app.documents_p128(like app.documents_p16 including all) partition by hash (org_id);
select format('create table app.documents_p128_%s partition of app.documents_p128 for values with (modulus 128, remainder %s)', i, i)
  from generate_series(0, 127) i \gexec

insert into app.documents_p16
  select i, 1 + (i % :orgs), (i % :users) + 1, md5(i::text), now() - (i || ' seconds')::interval
  from generate_series(1, :docs) i;
insert into app.documents_p128 select * from app.documents_p16;
create index on app.documents_p16(created_at);
create index on app.documents_p128(created_at);
