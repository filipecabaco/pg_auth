-- The engine's rules, kept in Postgres in the engine's own language: the OpenFGA model (its DSL),
-- the SpiceDB schema, the Cerbos policies (YAML). Each change is a new version, kept for review
-- and history; the sidecar pushes the latest to the engine and records whether it took it.
--
--   select authz.set_rules('openfga', 'model', $$ model ... $$);
--   select authz.set_rules('cerbos', 'resource/expense', $$ apiVersion: api.cerbos.dev/v1 ... $$);
--   select name, version, pushed_at, error from authz.rules();
--
-- OpenFGA and SpiceDB have one model or schema: one name. Cerbos has a policy per file: one name
-- each, as you would name the files. Optional: rules managed in the engine itself stay there.

create table authz.rules (
    engine     text not null check (engine in ('openfga', 'spicedb', 'cerbos')),
    name       text not null,
    version    int not null,
    body       text not null,
    created_at timestamptz not null default now(),
    created_by name not null default current_user,
    -- when the engine took it, or why it refused it (an invalid model, a policy that does not parse)
    pushed_at  timestamptz,
    error      text,
    primary key (engine, name, version)
);

-- Store a new version of the rules called `name` for `engine` (none when the text is unchanged)
-- and have the sidecar push it. Returns the version in force.
create function authz.set_rules(engine text, name text, body text) returns int
language plpgsql volatile as $$
#variable_conflict use_column
declare
    latest authz.rules;
    other text;
begin
    if engine not in ('openfga', 'spicedb', 'cerbos') then
        raise exception 'rules are for openfga, spicedb or cerbos (got %)', engine;
    end if;
    if engine in ('openfga', 'spicedb') then
        select r.name into other from authz.rules r where r.engine = set_rules.engine and r.name <> set_rules.name limit 1;
        if other is not null then
            raise exception '% has one %, already stored as %', engine,
                case engine when 'openfga' then 'model' else 'schema' end, quote_literal(other)
                using hint = format('select authz.set_rules(%L, %L, ...) replaces it', engine, other);
        end if;
    end if;
    select * into latest from authz.rules r where r.engine = set_rules.engine and r.name = set_rules.name order by r.version desc limit 1;
    if latest.body = set_rules.body then
        return latest.version;
    end if;
    insert into authz.rules (engine, name, version, body) values (engine, name, coalesce(latest.version, 0) + 1, body);
    perform pg_notify('pg_auth_rules', engine);
    return coalesce(latest.version, 0) + 1;
end
$$;

-- The rules in force: the latest version of each, whether the engine took it, and why not.
create function authz.rules(engine text default null)
returns table (engine text, name text, version int, body text, pushed_at timestamptz, error text)
language sql stable as $$
    select distinct on (r.engine, r.name) r.engine, r.name, r.version, r.body, r.pushed_at, r.error
    from authz.rules r
    where rules.engine is null or r.engine = rules.engine
    order by r.engine, r.name, r.version desc
$$;

-- For the sidecar: the latest version of each of `p_engine`'s rules not pushed yet, to the one
-- sidecar pushing (an advisory lock held until the caller's transaction ends).
create function authz.rules_pending(p_engine text) returns table (name text, version int, body text)
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
begin
    if not pg_try_advisory_xact_lock(hashtext('pg_auth.rules')) then
        return;
    end if;
    return query select l.name, l.version, l.body from authz.rules(p_engine) l where l.pushed_at is null and l.error is null;
end
$$;

-- For the sidecar: the engine took this version (no error), or refused it for good (the error).
create function authz.rules_pushed(p_engine text, p_name text, p_version int, p_error text default null) returns void
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    update authz.rules set pushed_at = case when p_error is null then now() end, error = p_error
    where engine = p_engine and name = p_name and version = p_version
$$;

revoke all on authz.rules from public;
revoke execute on function authz.set_rules(text, text, text) from public;
revoke execute on function authz.rules(text) from public;
revoke execute on function authz.rules_pending(text) from public;
revoke execute on function authz.rules_pushed(text, text, int, text) from public;
