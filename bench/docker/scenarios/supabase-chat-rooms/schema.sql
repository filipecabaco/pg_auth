-- psql variable :scale = number of messages. Loaded once per container.
select greatest(:scale / 50, 40)::int as rooms,
       greatest(:scale / 20, 100)::int as users \gset

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
-- user number n as a uuid, so subject ids are readable: ...0001, ...0002
create function app.u(n int) returns uuid language sql immutable as
  $$ select ('00000000-0000-0000-0000-' || lpad(n::text, 12, '0'))::uuid $$;

create table app.rooms(id int primary key, name text not null, is_public boolean not null);
insert into app.rooms select r, 'room-' || r, (r % 100 = 3) from generate_series(1, :rooms) r;

create table app.room_members(room_id int not null, user_id uuid not null, role text not null,
                              primary key (room_id, user_id));
-- noise users (4..): two rooms each
insert into app.room_members select 1 + (u::bigint * 7919) % :rooms, app.u(u), 'member' from generate_series(4, :users) u on conflict do nothing;
insert into app.room_members select 1 + (u::bigint * 104729) % :rooms, app.u(u), 'member' from generate_series(4, :users) u on conflict do nothing;
-- every 10th user joins the big public room 3
insert into app.room_members select 3, app.u(u), 'member' from generate_series(10, :users, 10) u on conflict do nothing;
-- the three named subjects
insert into app.room_members values (1, app.u(1), 'member'), (2, app.u(1), 'member');
insert into app.room_members select r, app.u(2), 'member' from generate_series(1, (:rooms * 0.4)::int) r on conflict do nothing;
insert into app.room_members values (1, app.u(3), 'member'), (3, app.u(3), 'moderator') on conflict (room_id, user_id) do update set role = excluded.role;
create index on app.room_members(user_id, room_id);

create table app.messages(id bigint primary key, room_id int not null, user_id uuid not null,
                          body text not null, created_at timestamptz not null);
insert into app.messages
  select i, case when i % 20 = 0 then 3 else 1 + (i * 7919::bigint) % :rooms end,
         app.u(4 + (i % (:users - 3))), md5(i::text), now() - (i || ' seconds')::interval
  from generate_series(1, :scale) i;
create index on app.messages(room_id, created_at desc, id desc);

create table app.reactions(id bigint primary key, message_id bigint not null, room_id int not null,
                           user_id uuid not null, emoji text not null);
-- room_id is denormalized from the message: pg_auth protects a row by one column of its own
insert into app.reactions
  select i, m.id, m.room_id, app.u(4 + (i % (:users - 3))), (array['+1','heart','laugh','eyes','tada'])[1 + i % 5]
  from generate_series(1, :scale) i join app.messages m on m.id = 1 + (i::bigint * 31) % :scale;
create index on app.reactions(message_id);
create index on app.reactions(room_id, emoji);  -- policy column + reaction_counts (index-only)
create index on app.reactions(user_id);

create table app.room_reads(room_id int not null, user_id uuid not null, last_read_at timestamptz not null,
                            primary key (user_id, room_id));
insert into app.room_reads
  select room_id, user_id, now() - ((abs(hashtext(user_id::text || room_id)) % :scale) || ' seconds')::interval
  from app.room_members;

create index on app.messages(user_id);
create index on app.room_members(room_id, role);  -- member list / moderator lookups
create index on app.rooms(id) where is_public;  -- partial: the few public rooms
create index on app.room_reads(room_id, user_id) include (last_read_at);
analyze;

-- helpers for the RLS variants
-- naive: what a Supabase docs / Slack-clone style policy calls for every row (security definer so the
-- policy on room_members does not recurse into itself)
create function app.can_see_room(rid int) returns boolean language plpgsql stable security definer as $f$
  begin
    return exists (select 1 from app.rooms r where r.id = rid and r.is_public)
        or exists (select 1 from app.room_members m where m.room_id = rid and m.user_id = auth.uid());
  end $f$;
-- best practice: the whole set of visible rooms once, as an array
create function app.visible_rooms() returns int[] language sql stable security definer as $f$
  select array(select id from app.rooms where is_public
               union select room_id from app.room_members where user_id = auth.uid()) $f$;
