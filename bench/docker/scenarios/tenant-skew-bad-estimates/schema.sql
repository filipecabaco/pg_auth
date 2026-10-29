-- psql variable :scale = number of events.
create schema app;
-- In this scenario the subject id IS the tenant id (user n belongs to tenant n only).
create function app.tenant() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.devices(id int primary key, model text not null);
insert into app.devices select i, 'm' || (i % 40) from generate_series(1, 20000) i;

create table app.events(id bigint generated always as identity primary key, tenant_id int not null, kind int not null,
                        device_id int not null, ts timestamptz not null, payload text);
insert into app.events (tenant_id, kind, device_id, ts, payload)
select case when g % 20 < 10 then 1 when g % 20 < 12 then 2 when g % 20 = 12 then 3 else 4 + (g % 1997) end,
       (g / 20) % 50, 1 + (g % 20000), now() - (g || ' seconds')::interval, md5(g::text)
from generate_series(1, :scale) g;
create index on app.events(tenant_id, ts desc);
create index on app.events(kind);
create index on app.events(device_id);
-- the tenant -> user mapping (user n is a member of tenant n)
create table app.tenant_users(tenant_id int primary key, user_id int not null);
insert into app.tenant_users select distinct tenant_id, tenant_id from app.events;
