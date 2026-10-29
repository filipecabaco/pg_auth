-- psql variable :scale = number of documents. Loaded once per container.
select :scale::bigint as docs, greatest(:scale / 2000, 20)::int as tenants \gset

create extension if not exists pg_trgm;
create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.memberships(user_id int, tenant_id int, primary key (user_id, tenant_id));
create index on app.memberships(tenant_id, user_id);
insert into app.memberships values (1, 1);
insert into app.memberships select 2, g from generate_series(1, 10) g;
insert into app.memberships select 0, t from generate_series(1, :tenants) t where t % 3 <> 0;
insert into app.memberships select 4, t from generate_series(1, :tenants) t where t % 6 <> 0;
insert into app.memberships select u, 1 + (u::bigint * 7919) % :tenants from generate_series(10, 10 + :tenants) u on conflict do nothing;

-- n skewed words 'termK' (K in 0..99999, density ~ K^(-2/3)) for row i; salt picks the word slots.
create function app.words(i bigint, salt int, n int) returns text language sql immutable as $f$
  select string_agg('term' || floor(100000 * power((hashint8(i * 64 + salt + j)::bigint + 2147483648) / 4294967296.0, 3))::int, ' ')
  from generate_series(0, n - 1) j $f$;

create table app.documents(id bigint primary key, tenant_id int not null, title text not null, body text not null, tsv tsvector not null);
insert into app.documents
  select i, 1 + (hashint8(i + 7)::bigint + 2147483648) % :tenants, x.t, x.b, to_tsvector('simple', x.t || ' ' || x.b)
  from generate_series(1, :docs) i, lateral (select app.words(i, 0, 5) t, app.words(i, 5, 15) b) x;

create index documents_tenant on app.documents(tenant_id);
create index documents_tsv on app.documents using gin (tsv);
create index documents_title_trgm on app.documents using gin (title gin_trgm_ops);

create function app.is_member_plpgsql(t int) returns boolean language plpgsql stable as $f$
  begin return exists (select 1 from app.memberships where tenant_id = t and user_id = app.uid()); end $f$;
