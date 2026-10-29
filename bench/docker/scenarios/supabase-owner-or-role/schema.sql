-- psql variable :scale = number of todos. Loaded once per container.
select :scale::bigint as todos,
       greatest(:scale / 100, 20)::int as users \gset

-- Supabase's own auth helpers (PostgREST sets request.jwt.claims on every request)
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
-- user number n as a uuid: ...0001 is the few_rows user, ...0002 many_rows, ...0003 admin
create function app.u(n int) returns uuid language sql immutable as
  $$ select ('00000000-0000-0000-0000-' || lpad(n::text, 12, '0'))::uuid $$;

-- who holds an app role. In Supabase this ends up in the JWT through the custom access token hook;
-- here the subjects' claims are fixed, and this table is the source for the OpenFGA tuples.
create table app.user_roles(user_id uuid primary key, role text not null);
insert into app.user_roles values (app.u(3), 'admin'), (app.u(5), 'support');

create table app.todos(id bigint primary key, owner_id uuid not null, title text not null,
                       done boolean not null, created_at timestamptz not null);
insert into app.todos
  select i,
         app.u(case when i % 10 = 0 then 2
                    when i % 2500 = 1 then 1
                    when i % 5000 = 2 then 3
                    else 10 + (i % (:users - 9)) end),
         md5(i::text), i % 3 = 0, now() - (i || ' seconds')::interval
  from generate_series(1, :todos) i;
-- indexes a real app would have
create index todos_owner_recent on app.todos(owner_id, created_at desc, id desc);  -- "my newest todos"
create index todos_recent on app.todos(created_at desc, id desc);                  -- admin/global feed
create index todos_open_recent on app.todos(created_at desc, id desc) where not done;  -- partial: open todos

-- items carry their todo's owner_id (denormalised, as multi-tenant schemas usually do)
create table app.todo_items(id bigint primary key, todo_id bigint not null, owner_id uuid not null, body text not null);
insert into app.todo_items
  select i, t.id, t.owner_id, 'item ' || i
  from generate_series(1, :todos) i join app.todos t on t.id = 1 + (i::bigint * 31) % :todos;
create index todo_items_todo on app.todo_items(todo_id, id);   -- join column
create index todo_items_owner on app.todo_items(owner_id, id); -- policy column
analyze;
