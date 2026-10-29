-- psql variable :scale = number of events. Loaded once per container.
select :scale::bigint as docs, greatest(:scale / 2500, 20)::int as tenants \gset

create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.memberships(user_id int, tenant_id int, primary key (user_id, tenant_id));
create index on app.memberships(tenant_id, user_id);
insert into app.memberships values (1, :tenants);                                              -- tiny
insert into app.memberships select 2, :tenants / 4 + g from generate_series(0, 9) g;           -- mid
insert into app.memberships values (3, 1);                                                     -- one big tenant
insert into app.memberships select 0, t from generate_series(1, :tenants) t where t % 3 <> 0;  -- many
insert into app.memberships select u, 1 + (u::bigint * 7919) % :tenants from generate_series(10, 10 + :tenants) u on conflict do nothing;

-- Time-ordered: id and created_at grow together (BRIN works), spread over 10 days ending 2026-10-01.
-- Tenant sizes are skewed: the share of tenant t is about (t/T)^(1/3) - ((t-1)/T)^(1/3).
create table app.events(id bigint primary key, tenant_id int not null, created_at timestamptz not null,
                        kind text not null, payload jsonb not null);
insert into app.events
  select i,
         1 + floor(:tenants * power((hashint8(i)::bigint + 2147483648) / 4294967296.0, 3))::int,
         timestamptz '2026-10-01' - (:docs - i) * (864000.0 / :docs) * interval '1 second',
         case when (i * 31) % 100 = 0 then 'error'
              else (array['view','click','login','search','purchase'])[1 + (i * 17) % 5] end,
         jsonb_build_object('plan', case when i % 500 = 0 then 'enterprise' else (array['free','pro','team'])[1 + i % 3] end,
                            'ok', i % 7 <> 0, 'n', i % 1000, 'ref', 'r' || (i % 50))
  from generate_series(1, :docs) i;

create index events_created_brin on app.events using brin (created_at) with (pages_per_range = 32);
create index events_tenant_created on app.events (tenant_id, created_at);
create index events_error_created on app.events (created_at) where kind = 'error';   -- partial: hot kind
create index events_cover on app.events (created_at) include (tenant_id, kind);      -- covering
create index events_payload on app.events using gin (payload jsonb_path_ops);

create function app.is_member_plpgsql(t int) returns boolean language plpgsql stable as $f$
  begin return exists (select 1 from app.memberships where tenant_id = t and user_id = app.uid()); end $f$;
