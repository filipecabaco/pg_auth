-- psql variable :scale = number of rows. Loaded once per container.
select :scale::bigint as docs, greatest(:scale / 5000, 20)::int as tenants \gset

create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

create table app.memberships(user_id int, tenant_id int, primary key (user_id, tenant_id));
create index on app.memberships(tenant_id, user_id);
insert into app.memberships values (1, 1);
insert into app.memberships select 2, g from generate_series(1, 10) g;
insert into app.memberships select 0, t from generate_series(1, :tenants) t where t % 2 = 0;
insert into app.memberships select u, 1 + (u::bigint * 7919) % :tenants from generate_series(10, 10 + :tenants) u on conflict do nothing;

-- Rows of a tenant are scattered over the heap (tenant = hash of id): the worst case for IO.
create table app.big(id bigint primary key, tenant_id int not null, val int not null, pad text not null);
insert into app.big
  select i, 1 + (hashint8(i)::bigint + 2147483648) % :tenants, (i % 1000)::int, md5(i::text) || md5((i + 1)::text)
  from generate_series(1, :docs) i;
create index big_tenant_id on app.big(tenant_id, id);

create function app.is_member_plpgsql(t int) returns boolean language plpgsql stable as $f$
  begin return exists (select 1 from app.memberships where tenant_id = t and user_id = app.uid()); end $f$;
