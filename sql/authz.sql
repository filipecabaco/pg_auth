-- pg_auth: enforce an external authorization engine's decisions inside Postgres.
--
-- Postgres stores no relationships and decides nothing. The source-of-truth engine (OpenFGA or
-- SpiceDB) answers through pg-auth-sidecar (sidecar/query), which the extension asks over a local
-- socket (src/sidecar.rs):
--   authz_internal.bindings      protected table + column (or key expression) -> object_type + permission
--   authz_internal.settings      how the current subject is identified
-- A bound table gets the policy authz.permits(type, permission, col::text). The planner hook
-- (src/rewrite/guard.rs) turns it into an indexable filter when the subject's allowed set is small.
--
-- Three schemas, by who uses them (sql/grants.sql grants them): authz, what people and queries
-- call; authz_sidecar, what the sidecar calls (role authz_sidecar); authz_internal, the tables and
-- the lower-level setup (role authz_admin).
create schema authz_internal;
create schema authz_sidecar;

create table authz_internal.bindings (
    rel              regclass not null,
    -- a table may carry several bindings (a row visible to its buyer, or to its seller's staff):
    -- each installs its own policies, named after it, and Postgres ORs permissive policies
    name             name not null default 'pg_auth',
    -- the row's object id: a column, or an IMMUTABLE expression over the row's columns (a path's
    -- first folder, a composite key), written as SQL and evaluated as `(key_expr)::text`
    column_name      name,
    key_expr         text,
    check ((column_name is null) <> (key_expr is null)),
    object_type      text collate "C" not null,
    permission       text collate "C" not null,
    write_permission text collate "C",
    -- an UPDATE or a DELETE needing another permission than other writes (GitHub: push to update,
    -- delete to delete); null: the write permission
    update_permission text collate "C",
    delete_permission text collate "C",
    -- INSERT checked against another object than the row's own (its parent: "may create in this
    -- project"), since a row's own object does not exist in the engine before the row does
    insert_object_type text collate "C",
    insert_column    name,
    insert_permission text collate "C",
    -- for an engine that decides on a resource's attributes (Cerbos): the columns sent as the
    -- row's attributes, by their names; the policy is then authz.permits_resource
    attributes       name[],
    -- attributes under another name than their column (Cerbos policies in camelCase: ownerId from
    -- owner_id), as {"name": "column"}
    attribute_map    jsonb,
    force            boolean not null default true,
    primary key (rel, name),
    check ((insert_object_type is null) = (insert_column is null) and (insert_column is null) = (insert_permission is null))
);

-- Drop the policies authz_internal.bind installed on p_rel, if any.
create function authz_internal.drop_policies(p_rel regclass) returns void
language plpgsql volatile as $$
declare
    pol name;
begin
    for pol in select polname from pg_policy where polrelid = p_rel and polname like 'pg\_auth\_%' loop
        execute format('drop policy %I on %s', pol, p_rel);
    end loop;
end
$$;

-- Install the policies for binding b on one relation (the bound table or one of its partitions).
create function authz_internal.apply_policies(p_part regclass, b authz_internal.bindings) returns void
language plpgsql volatile as $$
declare
    coltype regtype;
    fn      text;
    pred    text;
    key     text;
    attrs   text;
    pol     name;
begin
    if b.key_expr is not null then
        -- checks the expression against this relation's columns; IMMUTABLE so that its value
        -- for a row never changes, which the listed filter and an index on it rely on
        if not authz_internal.key_is_immutable(p_part, b.key_expr) then
            raise exception 'the key expression of % must be IMMUTABLE: %', p_part, b.key_expr;
        end if;
        key := format('(%s)', b.key_expr);
    else
        select a.atttypid::regtype into coltype
        from pg_attribute a
        where a.attrelid = p_part and a.attname = b.column_name and not a.attisdropped;

        if coltype is null then
            raise exception 'column % does not exist on %', b.column_name, p_part;
        end if;

        fn := case coltype
            when 'integer'::regtype then 'allowed_int4'
            when 'bigint'::regtype  then 'allowed_int8'
            when 'uuid'::regtype    then 'allowed_uuid'
            when 'text'::regtype    then 'allowed_text'
        end;
        if fn is null then
            raise exception 'unsupported column type % for %.% (supported: integer, bigint, uuid, text)',
                coltype, p_part, b.column_name;
        end if;
        key := quote_ident(b.column_name);
    end if;

    execute format('alter table %s enable row level security', p_part);
    if b.force then
        execute format('alter table %s force row level security', p_part);
    end if;
    -- this binding's policies only: the table's other bindings keep theirs
    for pol in select polname from pg_policy where polrelid = p_part
               and polname in (b.name || '_select', b.name || '_insert', b.name || '_update', b.name || '_delete') loop
        execute format('drop policy %I on %s', pol, p_part);
    end loop;

    -- the row's attributes, for an engine that decides on them: each a column, under its own name or
    -- the one attribute_map gives it
    if b.attributes is not null or b.attribute_map is not null then
        if exists (select 1 from (select unnest(b.attributes) union all select e.value from jsonb_each_text(b.attribute_map) e) c(col)
                   where not exists (select 1 from pg_attribute where attrelid = p_part and attname = c.col and attnum > 0 and not attisdropped)) then
            raise exception 'the attributes name a column % does not have: % %', p_part, b.attributes, b.attribute_map;
        end if;
        attrs := format('jsonb_build_object(%s)', (select string_agg(format('%L, %I', k, c), ', ') from (
                   select a, a from unnest(b.attributes) a union all select e.key, e.value from jsonb_each_text(b.attribute_map) e) p(k, c)));
    end if;

    -- The engine decides, through the sidecar. The planner hook turns this into an indexable
    -- filter when the subject's allowed set is small (src/rewrite/guard.rs).
    execute format('create policy %I on %s for select using (%s)', b.name || '_select', p_part, authz_internal.predicate(b.object_type, b.permission, key, attrs));

    if b.write_permission is not null and b.insert_object_type is null then
        pred := authz_internal.predicate(b.object_type, b.write_permission, key, attrs);
        execute format('create policy %I on %s for insert with check (%s)', b.name || '_insert', p_part, pred);
    end if;
    if coalesce(b.update_permission, b.write_permission) is not null then
        pred := authz_internal.predicate(b.object_type, coalesce(b.update_permission, b.write_permission), key, attrs);
        execute format('create policy %I on %s for update using (%s) with check (%s)', b.name || '_update', p_part, pred, pred);
    end if;
    if coalesce(b.delete_permission, b.write_permission) is not null then
        pred := authz_internal.predicate(b.object_type, coalesce(b.delete_permission, b.write_permission), key, attrs);
        execute format('create policy %I on %s for delete using (%s)', b.name || '_delete', p_part, pred);
    end if;
    if b.insert_object_type is not null then
        execute format('create policy %I on %s for insert with check (authz.permits(%L, %L, %I::text))',
                       b.name || '_insert', p_part, b.insert_object_type, b.insert_permission, b.insert_column);
    end if;
end
$$;

-- A policy's predicate: the engine's decision for the row's object id, and its attributes when
-- the binding has them.
create function authz_internal.predicate(p_type text, p_permission text, p_key text, p_attrs text) returns text
language sql immutable as $$
    select case when p_attrs is null then format('authz.permits(%L, %L, %s::text)', p_type, p_permission, p_key)
                else format('authz.permits_resource(%L, %L, %s::text, %s)', p_type, p_permission, p_key, p_attrs) end
$$;

-- Protect p_rel: rows are visible when p_column holds an object id on which the current subject
-- has p_permission. Writes are allowed only with p_write_permission; without it they are denied.
--
-- Partitions get the same policies, now and when they are created or attached later, since
-- querying a partition directly skips the parent's policies. With p_force (the default) the
-- table owner is subject to the policies too; maintenance jobs then need a BYPASSRLS role.
create function authz_internal.bind(
    p_rel regclass,
    p_column name,
    p_object_type text,
    p_permission text,
    p_write_permission text default null,
    p_force boolean default true,
    p_key text default null,
    p_name text default null,
    p_insert_object_type text default null,
    p_insert_column name default null,
    p_insert_permission text default null,
    p_attributes name[] default null,
    p_update_permission text default null,
    p_delete_permission text default null,
    p_attribute_map jsonb default null
) returns void
language plpgsql volatile as $$
declare
    b    authz_internal.bindings;
    part regclass;
begin
    insert into authz_internal.bindings (rel, name, column_name, key_expr, object_type, permission, write_permission,
                                insert_object_type, insert_column, insert_permission, force, attributes,
                                update_permission, delete_permission, attribute_map)
    values (p_rel, coalesce('pg_auth_' || p_name, 'pg_auth'), case when p_key is null then p_column end, p_key,
            p_object_type, p_permission, p_write_permission, p_insert_object_type, p_insert_column, p_insert_permission, p_force,
            -- a map alone still makes the binding decided on attributes
            case when p_attribute_map is not null then coalesce(p_attributes, '{}') else p_attributes end,
            p_update_permission, p_delete_permission, p_attribute_map)
    on conflict (rel, name) do update
        set column_name = excluded.column_name,
            key_expr = excluded.key_expr,
            insert_object_type = excluded.insert_object_type,
            insert_column = excluded.insert_column,
            insert_permission = excluded.insert_permission,
            object_type = excluded.object_type,
            permission = excluded.permission,
            write_permission = excluded.write_permission,
            update_permission = excluded.update_permission,
            delete_permission = excluded.delete_permission,
            attribute_map = excluded.attribute_map,
            force = excluded.force,
            attributes = excluded.attributes
    returning * into b;

    perform set_config('authz.applying', 'on', true);
    for part in select p_rel union select relid from pg_partition_tree(p_rel) loop
        perform authz_internal.apply_policies(part, b);
    end loop;
    perform set_config('authz.applying', '', true);
end
$$;

-- Stop protecting p_rel with pg_auth. Row level security stays enabled, so with no other
-- policies the table denies all access (fail closed).
create function authz_internal.unbind(p_rel regclass) returns void
language plpgsql volatile as $$
declare
    part regclass;
begin
    for part in select p_rel union select relid from pg_partition_tree(p_rel) loop
        perform authz_internal.drop_policies(part);
    end loop;
    delete from authz_internal.bindings where rel = p_rel;
    raise notice 'row level security is still enabled on %; it denies all rows until other policies are added', p_rel;
end
$$;

-- Partitions created or attached after authz_internal.bind inherit the binding's policies. Runs as the
-- extension owner, since callers cannot read authz_internal.bindings.
create function authz_internal.on_ddl() returns event_trigger
language plpgsql security definer set search_path = pg_catalog, pg_temp as $$
declare
    b    authz_internal.bindings;
    part regclass;
begin
    -- apply_policies runs ALTER TABLE, which fires this trigger again
    if current_setting('authz.applying', true) = 'on' then
        return;
    end if;
    perform set_config('authz.applying', 'on', true);

    for b in
        select distinct on (bi.rel, bi.name) bi.*
        from pg_event_trigger_ddl_commands() cmd
        join authz_internal.bindings bi
          on cmd.objid in (select relid from pg_partition_tree(bi.rel))
          or bi.rel in (select relid from pg_partition_ancestors(cmd.objid))
        where cmd.object_type in ('table', 'table column')
    loop
        for part in
            select t.relid from pg_partition_tree(b.rel) t
            where not exists (select 1 from pg_policy p
                              where p.polrelid = t.relid and p.polname = b.name || '_select')
        loop
            perform authz_internal.apply_policies(part, b);
        end loop;
    end loop;
    perform set_config('authz.applying', '', true);
end
$$;

create event trigger pg_auth_protect_partitions on ddl_command_end
    when tag in ('CREATE TABLE', 'ALTER TABLE')
    execute function authz_internal.on_ddl();

-- How the current subject is identified. Exactly one source is used:
--   'guc'         pg_auth.subject_id (superuser-only; GRANT SET ON PARAMETER to the API role)
--   'jwt'         a claim from request.jwt.claims; trusts whoever can set that setting
--   'jwt_signed'  a claim from a bearer token verified against jwks (HS256 / ES256)
create table authz_internal.settings (
    key   text primary key,
    value text not null
);
insert into authz_internal.settings values
    ('subject_source', 'guc'),
    ('subject_type', 'user'),
    ('claim', 'sub'),
    -- the principal's roles, for an engine that decides on them (Cerbos): Supabase sets role
    ('roles_claim', 'role'),
    -- Cerbos's scope (a tenant claim) and policy version: none by default, so scoped policies need one
    ('scope_claim', ''),
    ('policy_version', ''),
    -- what each request gives OpenFGA conditions and SpiceDB caveats: {"parameter": source}, a
    -- source being "now" (the time the engine is asked), {"claim": "region"} (the token's claim) or
    -- {"setting": "request.headers", "key": "x-forwarded-for"} (a setting, or a key of its JSON)
    ('context', ''),
    ('claims_setting', 'request.jwt.claims'),
    ('token_setting', 'request.headers'),
    ('token_header', 'authorization'),
    ('clock_skew_seconds', '30'),
    ('version', '1');

create function authz.configure(
    subject text default null,
    subject_type text default null,
    claim text default null,
    jwks jsonb default null,
    claims_setting text default null,
    token_setting text default null,
    token_header text default null,
    clock_skew_seconds int default null,
    roles_claim text default null,
    scope_claim text default null,
    policy_version text default null,
    context jsonb default null
) returns void
language plpgsql volatile as $$
declare
    p text;
    source jsonb;
begin
    for p, source in select key, value from jsonb_each(coalesce(configure.context, '{}')) loop
        if not (source = '"now"' or (jsonb_typeof(source) = 'object' and (source ?| array['claim', 'setting'])
                                     and not exists (select 1 from jsonb_object_keys(source) k where k not in ('claim', 'setting', 'key')))) then
            raise exception 'context %: a source is "now", {"claim": ...} or {"setting": ..., "key": ...} (got %)', p, source;
        end if;
    end loop;
    if subject is not null and subject not in ('guc', 'jwt', 'jwt_signed') then
        raise exception 'subject must be one of guc, jwt, jwt_signed (got %)', subject;
    end if;
    if subject = 'jwt_signed' and jwks is null
       and not exists (select 1 from authz_internal.settings where key = 'jwks') then
        raise exception 'subject => ''jwt_signed'' needs jwks => ''{"keys": [...]}''';
    end if;
    if jwks is not null and jsonb_typeof(jwks -> 'keys') is distinct from 'array' then
        raise exception 'jwks must be a JSON Web Key Set: {"keys": [...]}';
    end if;

    insert into authz_internal.settings (key, value)
    select k, v from (values
        ('subject_source', subject),
        ('subject_type', subject_type),
        ('claim', claim),
        ('jwks', jwks::text),
        ('claims_setting', claims_setting),
        ('token_setting', token_setting),
        ('token_header', token_header),
        ('clock_skew_seconds', clock_skew_seconds::text),
        ('roles_claim', roles_claim),
        ('scope_claim', scope_claim),
        ('policy_version', policy_version),
        ('context', case when configure.context = '{}' then '' else configure.context::text end)
    ) as s (k, v)
    where v is not null
    on conflict (key) do update set value = excluded.value;

    -- the statement trigger on authz_internal.settings makes every backend reload it
    update authz_internal.settings set value = (value::bigint + 1)::text where key = 'version';
end
$$;

grant usage on schema authz to public;
revoke execute on function authz.configure(text, text, text, jsonb, text, text, text, int, text, text, text, jsonb) from public;
