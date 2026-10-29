-- psql variable :scale = number of storage.objects rows. Loaded once per container.
select greatest(:scale / 50, 100)::int as users,
       greatest(:scale / 500, 10)::int as teams \gset

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

-- Supabase's own helper (storage-api migration 0002): plpgsql, no volatility marking => VOLATILE
create schema storage;
create function storage.foldername(name text) returns text[] language plpgsql as $f$
declare _parts text[];
begin
  select string_to_array(name, '/') into _parts;
  return _parts[1:array_length(_parts, 1) - 1];
end $f$;

create table app.team_members(team_id int not null, user_id uuid not null, primary key (team_id, user_id));
insert into app.team_members select 1 + (u::bigint * 7919) % :teams, app.u(u) from generate_series(4, :users) u on conflict do nothing;
insert into app.team_members select 1 + (u::bigint * 104729) % :teams, app.u(u) from generate_series(4, :users) u on conflict do nothing;
insert into app.team_members values (1, app.u(1)), (1, app.u(3));
insert into app.team_members select t, app.u(2) from generate_series(1, (:teams * 0.4)::int) t on conflict do nothing;
create index on app.team_members(user_id, team_id);

-- storage.objects as Supabase has it (reduced), plus acl_key for pg_auth
create table storage.objects(
  id uuid primary key default gen_random_uuid(),
  bucket_id text not null,
  name text not null,
  owner uuid,
  size bigint not null,
  created_at timestamptz not null,
  -- pg_auth protects a row by ONE column that names the object deciding access. Policies read the
  -- path; pg_auth cannot, so the path rule is repeated here: public bucket, or bucket + first folder.
  acl_key text generated always as
    (case when bucket_id = 'public-assets' then 'public-assets'
          else bucket_id || '_' || split_part(name, '/', 1) end) stored
);
-- category by i % 10: 0-5 user-files, 6-7 team-files, 8-9 public-assets
insert into storage.objects(bucket_id, name, owner, size, created_at)
select 'user-files',
       o::text || '/proj' || ((i / :users) % 5) || '/f' || i || '.png', o, 1000 + i % 5000,
       now() - (i || ' seconds')::interval
from (select i, app.u(case when i % 25 = 0 then 3 when i % 1000 = 1 then 1 when i % 1000 = 2 then 2
                           else 4 + i % (:users - 3) end) as o
      from generate_series(1, :scale) i where i % 10 < 6) s;
insert into storage.objects(bucket_id, name, owner, size, created_at)
select 'team-files', (1 + i % :teams) || '/proj' || ((i / :teams) % 5) || '/file' || i || '.pdf',
       app.u(4 + i % (:users - 3)), 1000 + i % 5000, now() - (i || ' seconds')::interval
from generate_series(1, :scale) i where i % 10 in (6, 7);
insert into storage.objects(bucket_id, name, owner, size, created_at)
select 'public-assets', (array['avatars','banners'])[1 + i % 2] || '/' || i || '.png',
       app.u(4 + i % (:users - 3)), 1000 + i % 5000, now() - (i || ' seconds')::interval
from generate_series(1, :scale) i where i % 10 in (8, 9);
-- one well-known file per named subject, for point lookups
insert into storage.objects(bucket_id, name, owner, size, created_at)
select 'user-files', app.u(n)::text || '/proj0/avatar.png', app.u(n), 100, now() from generate_series(1, 3) n;

-- Indexes a real app has (storage-api's own: unique (bucket_id, name), name text_pattern_ops)
create unique index bucketid_objname on storage.objects(bucket_id, name);
create index name_prefix_search on storage.objects(name text_pattern_ops);
-- prefix listings: range scans need C-collation order
create index objects_bucket_name_c on storage.objects(bucket_id, (name collate "C"));
create index objects_owner on storage.objects(owner);            -- "my uploads" counts
create index objects_created on storage.objects(created_at desc, id);  -- recent uploads feed
-- policy / pg_auth columns
create index objects_acl_key on storage.objects(acl_key);
-- immutable rewrite of the path rule, for best_practice_indexed_rls
create index objects_user_folder on storage.objects(bucket_id, (split_part(name, '/', 1)));
create index objects_team_folder on storage.objects(((split_part(name, '/', 1))::int)) where bucket_id = 'team-files';
analyze;

-- helpers for the RLS variants
create function app.my_teams() returns int[] language sql stable security definer as $f$
  select array(select team_id from app.team_members where user_id = auth.uid()) $f$;
