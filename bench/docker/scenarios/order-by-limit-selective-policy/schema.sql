-- psql variable :scale = number of documents.
select :scale::bigint as docs,
       greatest(:scale / 200, 20)::int as orgs \gset

create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
create index on app.memberships(org_id, user_id);
-- subject 1: only the oldest org. subject 2: every 20th org. subject 0: newest half.
insert into app.memberships values (1, :orgs);
insert into app.memberships select 2, o from generate_series(1, :orgs, 20) o;
insert into app.memberships select 0, o from generate_series(1, :orgs / 2) o;
-- background users
insert into app.memberships select u, 1 + (u::bigint * 7919) % :orgs from generate_series(3, :orgs) u on conflict do nothing;

-- Org o owns a contiguous time block: org 1 holds the newest documents, the last org the oldest.
create table app.documents(id bigint primary key, org_id int not null, title text not null, created_at timestamptz not null);
insert into app.documents
  select i, 1 + ((i - 1)::bigint * :orgs / :docs)::int, md5(i::text), now() - (i || ' seconds')::interval
  from generate_series(1, :docs) i;
create index on app.documents(org_id);
create index on app.documents(created_at);

create function app.my_orgs() returns int[] language sql stable security definer as $f$
  select array(select org_id from app.memberships where user_id = app.uid()) $f$;
