-- :scale = number of folders (3-ary tree, ~10 levels at 50k), files = 2 x folders,
-- plus scale/50 "noise" users with 1-2 random folder grants each.
select :scale::int as nf, (:scale * 2)::bigint as nfiles, greatest(:scale / 50, 10)::int as users \gset

create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.folders(id int primary key, parent_id int references app.folders);
-- 3-ary tree: parent of g is (g-2)/3+1 (integer division); folder 1 is the root. depth ~ log3(n).
insert into app.folders select g, case when g = 1 then null else (g - 2) / 3 + 1 end from generate_series(1, :nf) g;
create index on app.folders(parent_id);

create table app.files(id bigint primary key, folder_id int not null, name text not null, created_at timestamptz not null);
insert into app.files select i, 1 + (i % :nf), 'f' || i, now() - (i || ' seconds')::interval from generate_series(1, :nfiles) i;
create index on app.files(folder_id);
create index on app.files(created_at);

create table app.folder_grants(user_id int not null, folder_id int not null, primary key (user_id, folder_id));
-- selective user 1: two mid-level folders (subtree ~ tens of folders); broad user 0: three of the root's children (~100% of the tree minus one third... see README)
insert into app.folder_grants values (1, 122), (1, 365), (0, 2), (0, 3);
insert into app.folder_grants select u, 1 + (u::bigint * 7919) % :nf from generate_series(2, :users) u on conflict do nothing;
insert into app.folder_grants select u, 1 + (u::bigint * 104729) % :nf from generate_series(2, :users, 2) u on conflict do nothing;

-- closure table (ancestor, descendant) for the best-practice variant, plus helper functions.
create table app.folder_closure(ancestor int not null, descendant int not null, primary key (ancestor, descendant));
with recursive t(anc, d) as (
  select id, id from app.folders
  union all select t.anc, f.id from t join app.folders f on f.parent_id = t.d)
insert into app.folder_closure select anc, d from t;
create index on app.folder_closure(descendant);

create function app.my_folders() returns int[] language sql stable security definer set search_path = '' as $f$
  select array(select c.descendant from app.folder_grants g join app.folder_closure c on c.ancestor = g.folder_id
               where g.user_id = app.uid()) $f$;
