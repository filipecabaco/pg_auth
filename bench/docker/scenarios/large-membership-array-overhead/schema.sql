-- psql variable :scale = number of documents. ACL-style per-document grants.
-- user 1: 10 grants; user 5000: every 20th doc (scale/20); user 9999: every 2nd doc (scale/2).
select :scale::bigint as docs \gset
create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.documents(id bigint primary key, title text not null, created_at timestamptz not null);
insert into app.documents select i, md5(i::text), now() - (i || ' seconds')::interval from generate_series(1, :docs) i;

create table app.doc_grants(user_id int not null, doc_id bigint not null, primary key (user_id, doc_id));
create index on app.doc_grants(doc_id);
insert into app.doc_grants select 1, g from generate_series(1, 10) g;
insert into app.doc_grants select 5000, g from generate_series(20, :docs, 20) g;
insert into app.doc_grants select 9999, g from generate_series(2, :docs, 2) g;

create function app.my_doc_ids() returns setof bigint language sql stable security definer set search_path = '' as $f$
  select doc_id from app.doc_grants where user_id = app.uid() $f$;
