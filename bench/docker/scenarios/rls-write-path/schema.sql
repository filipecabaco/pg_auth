-- psql variable :scale = number of tasks.
select :scale::bigint as tks, greatest(:scale / 500, 20)::int as nt, least(:scale / 20, 5000)::bigint as drs \gset

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
create function app.tenant_idx(i bigint, nt int) returns int language sql immutable as
  $$ select case when i % 10 in (2,3,4) then 0 else 1 + ((i - 1) % (nt - 1))::int end $$;
create function app.tenant_uuid(t int) returns uuid language sql immutable as $$ select md5('tenant-' || t)::uuid $$;
create function app.user_uuid(t int, k int) returns uuid language sql immutable as $$ select md5('user-' || t || '-' || k)::uuid $$;
create function app.project_uuid(t int, j int) returns uuid language sql immutable as $$ select md5('project-' || t || '-' || j)::uuid $$;

-- Runs one statement that returns a single text value; a policy violation (42501) becomes 'denied'.
-- Lets insert queries have the same result in every variant, denied or not.
create function app.attempt(q text) returns text language plpgsql as $f$
declare r text;
begin
  execute q into r;
  return r;
exception when insufficient_privilege then return 'denied';
end $f$;

create table app.members(user_id uuid not null, tenant_id uuid not null, role text not null, primary key (user_id, tenant_id));
create index on app.members(tenant_id);
insert into app.members
  select app.user_uuid(t, k), app.tenant_uuid(t), case when k = 2 then 'viewer' else 'editor' end
  from generate_series(0, :nt - 1) t, generate_series(0, 2) k;

create table app.projects(id uuid primary key, tenant_id uuid not null, name text not null, archived boolean not null);
create index on app.projects(tenant_id);
insert into app.projects select app.project_uuid(t, j), app.tenant_uuid(t), 'project ' || t || '-' || j, j = 4
  from generate_series(0, :nt - 1) t, generate_series(0, 4) j;

create table app.tasks(id bigint primary key, tenant_id uuid not null, project_id uuid not null, owner_id uuid not null,
                       title text not null, status text not null, created_at timestamptz not null);
insert into app.tasks
  select i, app.tenant_uuid(t), app.project_uuid(t, (i % 5)::int), app.user_uuid(t, (i % 3)::int),
         md5(i::text), 'open', now() - (i || ' seconds')::interval
  from generate_series(1, :tks) i, lateral (select app.tenant_idx(i, :nt) t) x;
-- policy column leads (listing a tenant's tasks newest first, and every tenant-scoped write)
create index tasks_tenant_created on app.tasks(tenant_id, created_at desc);
-- UPDATE ... FROM projects joins on project_id and filters status = 'open'
create index tasks_project_open on app.tasks(project_id) where status = 'open';
-- owner_id = auth.uid() updates
create index tasks_owner on app.tasks(owner_id);

-- Small table used to probe INSERT authorisation by the row's own object id (see the README)
create table app.drafts(id bigint primary key, tenant_id uuid not null, title text not null);
create index on app.drafts(tenant_id);
insert into app.drafts select i, app.tenant_uuid(app.tenant_idx(i, :nt)), md5(i::text) from generate_series(1, :drs) i;
