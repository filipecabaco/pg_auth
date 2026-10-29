-- psql variable :scale = number of events. Tenant 0 is large (30% of events); tenants 1..nt-1 share the rest.
select :scale::bigint as evs, greatest(:scale / 500, 20)::int as nt, (:scale / 2)::bigint as nts \gset

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
-- tenant of row i: tenant 0 for 30% of rows, else 1 + (i-1) mod (nt-1). Event 1 is in tenant 1, event 2 in tenant 0.
create function app.tenant_idx(i bigint, nt int) returns int language sql immutable as
  $$ select case when i % 10 in (2,3,4) then 0 else 1 + ((i - 1) % (nt - 1))::int end $$;
create function app.tenant_uuid(t int) returns uuid language sql immutable as $$ select md5('tenant-' || t)::uuid $$;
create function app.user_uuid(t int, k int) returns uuid language sql immutable as $$ select md5('user-' || t || '-' || k)::uuid $$;
create function app.project_uuid(t int, j int) returns uuid language sql immutable as $$ select md5('project-' || t || '-' || j)::uuid $$;

create table app.members(user_id uuid not null, tenant_id uuid not null, primary key (user_id, tenant_id));
create index on app.members(tenant_id);
insert into app.members select app.user_uuid(t, k), app.tenant_uuid(t) from generate_series(0, :nt - 1) t, generate_series(0, 2) k;

create table app.projects(id uuid primary key, tenant_id uuid not null, name text not null);
create index on app.projects(tenant_id);
insert into app.projects select app.project_uuid(t, j), app.tenant_uuid(t), 'project ' || t || '-' || j
  from generate_series(0, :nt - 1) t, generate_series(0, 4) j;

create table app.events(id bigint primary key, tenant_id uuid not null, project_id uuid not null,
                        created_at timestamptz not null, payload text not null);
insert into app.events
  select i, app.tenant_uuid(t), app.project_uuid(t, (i % 5)::int), now() - (i || ' seconds')::interval, md5(i::text)
  from generate_series(1, :evs) i, lateral (select app.tenant_idx(i, :nt) t) x;
-- policy column leads: page query (tenant_id = X order by created_at desc limit 20) is one index range scan, no sort
create index events_tenant_created on app.events(tenant_id, created_at desc);
-- join key (tenant_id, project_id) for the events x projects join
create index events_tenant_project on app.events(tenant_id, project_id);

-- one owner per row: the owner_id = (select auth.uid()) pattern
create table app.notes(id bigint primary key, owner_id uuid not null, created_at timestamptz not null, body text not null);
insert into app.notes
  select i, app.user_uuid(t, (i % 3)::int), now() - (i || ' seconds')::interval, md5(i::text)
  from generate_series(1, :nts) i, lateral (select app.tenant_idx(i, :nt) t) x;
create index notes_owner_created on app.notes(owner_id, created_at desc);

-- Partition-per-tenant-group copy of events: hash partitions on the tenant key
create table app.events_p(id bigint not null, tenant_id uuid not null, project_id uuid not null,
                          created_at timestamptz not null, payload text not null,
                          primary key (tenant_id, id)) partition by hash (tenant_id);
select format('create table app.events_p_%s partition of app.events_p for values with (modulus 16, remainder %s)', i, i)
  from generate_series(0, 15) i \gexec
insert into app.events_p select * from app.events;
create index events_p_tenant_created on app.events_p(tenant_id, created_at desc);
