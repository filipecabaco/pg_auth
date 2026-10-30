-- Who may use what, by schema rather than function by function: a function added to
-- authz_internal or authz_sidecar is closed to everyone until a role is granted it.
--
--   authz           usage for every role: the functions queries call (permits, the allowed_*,
--                   indexed_*, plan_* and hierarchy_* helpers), and what people call. Its admin
--                   functions are revoked from public where they are made, and granted below.
--   authz_sidecar   the sidecar's: grant authz_sidecar to the sidecar's login.
--   authz_internal  the tables and the lower-level setup (protect, relate, setup): grant
--                   authz_admin to whoever runs migrations. Its functions run as their caller, so
--                   protecting a table still needs owning it.
--
-- Roles belong to the cluster, not the database: a second database's CREATE EXTENSION finds them,
-- and DROP EXTENSION leaves them.
do $$
begin
    if not exists (select from pg_roles where rolname = 'authz_admin') then
        create role authz_admin nologin;
    end if;
    if not exists (select from pg_roles where rolname = 'authz_sidecar') then
        create role authz_sidecar nologin;
    end if;
end
$$;

-- What a logical dump (pg_dump) carries with the extension: the configuration, and changes not yet
-- delivered. Not the permission index or the feed's position, which are derived and made again.
select pg_catalog.pg_extension_config_dump('authz_internal.settings', '');
select pg_catalog.pg_extension_config_dump('authz_internal.bindings', '');
select pg_catalog.pg_extension_config_dump('authz_internal.relationships', '');
select pg_catalog.pg_extension_config_dump('authz_internal.hierarchies', '');
select pg_catalog.pg_extension_config_dump('authz_internal.rules', '');
select pg_catalog.pg_extension_config_dump('authz_internal.setups', '');
select pg_catalog.pg_extension_config_dump('authz_internal.setups_version_seq', '');
select pg_catalog.pg_extension_config_dump('authz_internal.outbox', '');
select pg_catalog.pg_extension_config_dump('authz_internal.outbox_id_seq', '');

revoke all on schema authz_internal, authz_sidecar from public;
revoke all on all tables in schema authz_internal from public;
revoke execute on all functions in schema authz_internal, authz_sidecar from public;

grant usage on schema authz_sidecar to authz_sidecar;
grant execute on all functions in schema authz_sidecar to authz_sidecar;

grant usage on schema authz_internal to authz_admin;
-- the configuration it writes through authz.configure, set_rules, protect, relate and setup (which
-- run as their caller); the permission index, the outbox and the feed's position only to read,
-- since only pg_auth's own functions write them
grant select, insert, update, delete on authz_internal.settings, authz_internal.bindings, authz_internal.relationships,
    authz_internal.hierarchies, authz_internal.rules, authz_internal.setups to authz_admin;
grant usage on sequence authz_internal.setups_version_seq to authz_admin;
grant select on authz_internal.permission_sets, authz_internal.permitted, authz_internal.outbox,
    authz_internal.feed_positions to authz_admin;
grant execute on all functions in schema authz_internal to authz_admin;
grant execute on function authz.set_rules(text, text, text), authz.rules(text),
    authz.configure(text, text, text, jsonb, text, text, text, int, text, text, text, jsonb),
    authz.check(text, text, text), authz.status(), authz.why(regclass, text, text, name, text[], text),
    authz.doctor(), authz.lint() to authz_admin;

-- Last, once every function it calls exists (the extension's own DDL fires it too): partitions
-- created or attached later take their table's policies (authz_internal.on_ddl).
create event trigger pg_auth_protect_partitions on ddl_command_end
    when tag in ('CREATE TABLE', 'ALTER TABLE')
    execute function authz_internal.on_ddl();
