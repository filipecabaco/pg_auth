-- psql variable :scale = rows per table (documents, comments, attachments).
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
insert into app.documents select i, 1 + (i % :orgs), md5(i::text) from generate_series(1, :docs) i;
create index on app.documents(org_id);

create table app.comments(id bigint primary key, document_id bigint not null, body text);
insert into app.comments select i, 1 + (i::bigint * 31) % :docs, 'c' || i from generate_series(1, :docs) i;
create index on app.comments(document_id);

create table app.attachments(id bigint primary key, comment_id bigint not null, name text);
insert into app.attachments select i, 1 + (i::bigint * 37) % :docs, 'a' || i from generate_series(1, :docs) i;
create index on app.attachments(comment_id);

-- naive helper-free policies need none; best-practice helpers (SECURITY DEFINER, owner bypasses RLS)
create function app.my_orgs() returns int[] language sql stable security definer as $f$
  select array(select org_id from app.memberships where user_id = app.uid()) $f$;
create function app.my_docs() returns bigint[] language sql stable security definer as $f$
  select array(select d.id from app.documents d where d.org_id = any (app.my_orgs())) $f$;
create function app.my_comments() returns bigint[] language sql stable security definer as $f$
  select array(select c.id from app.comments c where c.document_id = any (app.my_docs())) $f$;
