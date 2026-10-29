-- psql variable :scale = number of messages. Loaded once per container.
select :scale::bigint as msgs,
       greatest(:scale / 200, 100)::int as orgs,
       greatest(:scale / 100, 100)::int as users \gset
select (:orgs * 4)::int as channels \gset

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
create function app.u(n int) returns uuid language sql immutable as
  $$ select ('00000000-0000-0000-0000-' || lpad(n::text, 12, '0'))::uuid $$;
-- the role a user holds in org o when the data does not say otherwise: guest, member, moderator, admin
create function app.role_in(o bigint) returns text language sql immutable as
  $$ select (array['guest', 'member', 'moderator', 'admin'])[1 + o % 4] $$;

-- Supabase RBAC guide: role -> permission
create table app.role_permissions(role text not null, permission text not null, primary key (role, permission));
insert into app.role_permissions values
  ('guest', 'channels.read'),
  ('member', 'channels.read'), ('member', 'messages.read'),
  ('moderator', 'channels.read'), ('moderator', 'messages.read'), ('moderator', 'messages.delete'),
  ('admin', 'channels.read'), ('admin', 'messages.read'), ('admin', 'messages.delete'), ('admin', 'channels.delete');

-- who holds which role in which org: the source of the JWT claims (custom access token hook) and
-- of the OpenFGA tuples. user ...01 is in orgs 61-64, ...02 in orgs 1-60 (their claims are in
-- scenario.json), the others in two orgs each.
create table app.org_members(user_id uuid not null, org_id int not null, role text not null,
                             primary key (user_id, org_id));
insert into app.org_members select app.u(1), o, app.role_in(o) from generate_series(61, 64) o;
insert into app.org_members select app.u(2), o, app.role_in(o) from generate_series(1, 60) o;
insert into app.org_members
  select app.u(10 + k), 1 + (k::bigint * 7919) % :orgs, app.role_in(1 + (k::bigint * 7919) % :orgs)
  from generate_series(0, :users - 1) k
  on conflict do nothing;
insert into app.org_members
  select app.u(10 + k), 1 + (k::bigint * 104729) % :orgs, app.role_in(1 + (k::bigint * 104729) % :orgs)
  from generate_series(0, :users - 1) k
  on conflict do nothing;
create index on app.org_members(org_id, user_id);

create table app.channels(id bigint primary key, org_id int not null, name text not null);
insert into app.channels select c, 1 + c % :orgs, 'channel-' || c from generate_series(1, :channels) c;
create index channels_org on app.channels(org_id, id);

create table app.messages(id bigint primary key, channel_id bigint not null, org_id int not null,
                          user_id uuid not null, body text not null, created_at timestamptz not null);
insert into app.messages
  select i, 1 + i % :channels, 1 + (1 + i % :channels) % :orgs, app.u(10 + i % :users),
         'message ' || i, now() - (i || ' seconds')::interval
  from generate_series(1, :msgs) i;
-- indexes a real chat app has: global feed, per-org feed, per-channel feed (also the join column)
create index messages_recent on app.messages(created_at desc, id desc);
create index messages_org_recent on app.messages(org_id, created_at desc, id desc);
create index messages_channel_recent on app.messages(channel_id, created_at desc, id desc);
analyze;
