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

revoke all on schema authz_internal, authz_sidecar from public;
revoke all on all tables in schema authz_internal from public;
revoke execute on all functions in schema authz_internal, authz_sidecar from public;

grant usage on schema authz_sidecar to authz_sidecar;
grant execute on all functions in schema authz_sidecar to authz_sidecar;

grant usage on schema authz_internal to authz_admin;
grant all on all tables in schema authz_internal to authz_admin;
grant usage on all sequences in schema authz_internal to authz_admin;
grant execute on all functions in schema authz_internal to authz_admin;
grant execute on function authz.set_rules(text, text, text), authz.rules(text),
    authz.configure(text, text, text, jsonb, text, text, text, int, text, text, text, jsonb),
    authz.check(text, text, text), authz.status(), authz.why(regclass, text, text, name, text[], text),
    authz.doctor(), authz.lint() to authz_admin;
