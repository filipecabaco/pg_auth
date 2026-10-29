-- :scale = number of documents. Folders: scale/50 in a 4-ary tree (parent of f is (f-2)/4+1).
-- Groups: 364 in a 3-ary tree of 6 levels (parent group g contains child groups; members of a child
-- group are members of every ancestor group). User n is app.u(n): 1 = direct, 2 = deep_group, 3 = big_group.
select :scale::int as nd, greatest(:scale / 50, 1000)::int as nf, greatest(:scale / 100, 100)::int as nu, 364 as ng \gset

create schema if not exists auth;
create or replace function auth.uid() returns uuid language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim.sub', true), ''),
                  (nullif(current_setting('request.jwt.claims', true), '')::jsonb ->> 'sub'))::uuid $$;
create or replace function auth.jwt() returns jsonb language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim', true), ''),
                  nullif(current_setting('request.jwt.claims', true), ''))::jsonb $$;
create or replace function auth.role() returns text language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim.role', true), ''),
                  (nullif(current_setting('request.jwt.claims', true), '')::jsonb ->> 'role'))::text $$;

create schema app;
create function app.u(n int) returns uuid language sql immutable as
  $$ select ('00000000-0000-4000-8000-' || lpad(to_hex(n), 12, '0'))::uuid $$;

create table app.groups(id int primary key, name text not null);
insert into app.groups select g, 'group ' || g from generate_series(1, :ng) g;

-- group_groups(parent_id, child_id): child is a member group of parent
create table app.group_groups(parent_id int not null, child_id int not null, primary key (parent_id, child_id));
insert into app.group_groups select (g - 2) / 3 + 1, g from generate_series(2, :ng) g;
create index on app.group_groups(child_id, parent_id);

create table app.group_users(group_id int not null, user_id uuid not null, primary key (group_id, user_id));
insert into app.group_users values (364, app.u(2)), (3, app.u(3));
insert into app.group_users select 1 + (u * 37) % :ng, app.u(u) from generate_series(10, :nu) u on conflict do nothing;
insert into app.group_users select 1 + (u * 91) % :ng, app.u(u) from generate_series(10, :nu) u on conflict do nothing;
create index on app.group_users(user_id, group_id);

create table app.folders(id int primary key, parent_id int references app.folders, name text not null);
insert into app.folders select f, case when f = 1 then null else (f - 2) / 4 + 1 end, 'folder ' || f from generate_series(1, :nf) f;
create index on app.folders(parent_id, id);

create table app.documents(id int primary key, folder_id int not null references app.folders, title text not null, owner_id uuid not null, updated_at timestamptz not null);
insert into app.documents
  select i, 1 + i % :nf,
    (array['Budget','Roadmap','Meeting notes','Contract','Invoice','Design review','Postmortem'])[1 + i % 7] || ' ' ||
      (array['Q1','Q2','Q3','Q4','2024','2025'])[1 + (i / 7) % 6] || ' #' || i,
    app.u(10 + (i * 13) % (:nu - 9)),
    now() - (((i::bigint * 7919) % 1000003) || ' seconds')::interval
  from generate_series(1, :nd) i;
-- listing a folder newest first, and the global newest-first feed
create index documents_folder_updated on app.documents(folder_id, updated_at desc, id);
create index documents_updated on app.documents(updated_at desc, id);

-- a share grants view on a folder (and so on its whole subtree) to one user or to one group
create table app.folder_shares(folder_id int not null, user_id uuid, group_id int, check ((user_id is null) <> (group_id is null)));
create unique index on app.folder_shares(folder_id, user_id) where user_id is not null;
create unique index on app.folder_shares(folder_id, group_id) where group_id is not null;
create index on app.folder_shares(user_id, folder_id) where user_id is not null;
create index on app.folder_shares(group_id, folder_id) where group_id is not null;
-- direct: user 1 on folder 100. deep_group: its leaf group and 4 ancestor groups. big_group: group 3 on folder 2.
insert into app.folder_shares(folder_id, user_id) values (100, app.u(1));
insert into app.folder_shares(folder_id, group_id) values (900, 364), (300, 121), (90, 40), (35, 13), (20, 4), (2, 3);
-- noise: every other group (except the special ones) shares one folder; many users have one direct share
insert into app.folder_shares(folder_id, group_id)
  select 1 + (g * 131) % :nf, g from generate_series(2, :ng) g where g not in (3, 4, 13, 40, 121, 364) on conflict do nothing;
insert into app.folder_shares(folder_id, user_id)
  select 1 + (u * 7919) % :nf, app.u(u) from generate_series(10, :nu) u on conflict do nothing;

-- closure tables for the best_practice variant and the baseline
create table app.folder_closure(ancestor int not null, descendant int not null, primary key (ancestor, descendant));
with recursive t(anc, d) as (
  select id, id from app.folders
  union all select t.anc, f.id from t join app.folders f on f.parent_id = t.d)
insert into app.folder_closure select anc, d from t;
create index on app.folder_closure(descendant, ancestor);

-- group_ancestors(group_id, ancestor_id): ancestor_id contains group_id (itself included)
create table app.group_ancestors(group_id int not null, ancestor_id int not null, primary key (group_id, ancestor_id));
with recursive t(g, a) as (
  select id, id from app.groups
  union all select t.g, gg.parent_id from t join app.group_groups gg on gg.child_id = t.a)
insert into app.group_ancestors select g, a from t;
create index on app.group_ancestors(ancestor_id, group_id);

-- folders visible to uid through closure tables (baseline queries use it; best_practice wraps it)
create function app.visible_folders_closure(uid uuid) returns setof int language sql stable as $$
  select c.descendant from app.folder_shares fs join app.folder_closure c on c.ancestor = fs.folder_id where fs.user_id = uid
  union
  select c.descendant from app.group_users gu
    join app.group_ancestors ga on ga.group_id = gu.group_id
    join app.folder_shares fs on fs.group_id = ga.ancestor_id
    join app.folder_closure c on c.ancestor = fs.folder_id
  where gu.user_id = uid $$;

-- the same set without closure tables: expand groups and the folder subtree with recursive CTEs
create function app.visible_folders_recursive(uid uuid) returns setof int language sql stable as $$
  with recursive ug(id) as (
    select group_id from app.group_users where user_id = uid
    union select gg.parent_id from app.group_groups gg join ug on gg.child_id = ug.id),
  f(id) as (
    select folder_id from app.folder_shares where user_id = uid
    union select folder_id from app.folder_shares where group_id in (select id from ug)
    union select ch.id from app.folders ch join f on ch.parent_id = f.id)
  select id from f $$;

create function app.my_folders_closure() returns int[] language sql stable security definer set search_path = '' as $$
  select array(select app.visible_folders_closure((select auth.uid()))) $$;
create function app.my_folders_recursive() returns int[] language sql stable security definer set search_path = '' as $$
  select array(select app.visible_folders_recursive((select auth.uid()))) $$;
-- the group half on its own, for the per-row policy
create function app.user_groups(uid uuid) returns setof int language sql stable security definer set search_path = '' as $$
  with recursive ug(id) as (
    select group_id from app.group_users where user_id = uid
    union select gg.parent_id from app.group_groups gg join ug on gg.child_id = ug.id)
  select id from ug $$;
-- ancestors of a folder (itself included). security definer: a policy on folders cannot read folders
-- itself without "infinite recursion detected in policy", so the walk has to be a definer function.
create function app.folder_ancestors(fid int) returns table(a int) language sql stable security definer set search_path = '' as $$
  with recursive anc(id) as (
    select fid
    union all select f.parent_id from app.folders f join anc on f.id = anc.id where f.parent_id is not null)
  select id from anc $$;
