-- psql variable :scale = number of documents. ~1000 docs per owner. NO index on owner_id in the
-- base schema (with an index the naive policy is evaluated once as an index condition).
select :scale::bigint as docs, greatest(:scale / 1000, 10)::int as users \gset

create schema app;
create table app.documents(id bigint primary key, owner_id int not null, title text not null, created_at timestamptz not null);
insert into app.documents
  select i, 1 + (i % :users), md5(i::text), now() - (i || ' seconds')::interval from generate_series(1, :docs) i;
