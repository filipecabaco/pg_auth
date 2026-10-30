-- Declared hierarchies (docs/proposals/permission-index.md, phase B): a permission inherited
-- along a tree Postgres already stores (a folder's parent_id). The engine is asked only for a
-- subject's direct grants; Postgres expands them down a transitive closure of the tree, which
-- triggers on the app's own table keep current, as Zanzibar's Leopard index does.
--
-- The model must match: <permission> = <direct permission> or <permission> from parent, with the
-- parent edges being the table's parent column (pg_auth cannot verify this; the bench's
-- hierarchical-permissions scenario compares both against the engine).

create table authz_internal.hierarchies (
    object_type       text collate "C" not null,
    permission        text collate "C" not null,
    direct_permission text collate "C" not null,
    source            regclass not null,
    id_column         name not null,
    parent_column     name not null,
    closure           regclass not null,
    -- bumped by every change to the tree, so expansions cached across transactions are dropped
    version           bigint not null default 0,
    primary key (object_type, permission)
);

-- Keep authz.closure_<source oid> (ancestor, descendant), including each node with itself, in
-- step with one row change of the source table. TG_ARGV: closure, id column, parent column, id type.
create function authz_internal.closure_maintain() returns trigger
language plpgsql security definer set search_path = pg_catalog, pg_temp as $$
declare
    c      text := tg_argv[0];
    idc    name := tg_argv[1];
    par    name := tg_argv[2];
    typ    text := tg_argv[3];
    old_id text;
    new_id text;
    new_parent text;
    old_parent text;
    cycle      boolean;
begin
    if tg_op in ('UPDATE', 'DELETE') then
        execute format('select ($1).%I::text, ($1).%I::text', idc, par) into old_id, old_parent using old;
    end if;
    if tg_op in ('INSERT', 'UPDATE') then
        execute format('select ($1).%I::text, ($1).%I::text', idc, par) into new_id, new_parent using new;
    end if;

    if tg_op = 'UPDATE' and old_id is not distinct from new_id and old_parent is not distinct from new_parent then
        return null;
    end if;

    if tg_op = 'DELETE' or (tg_op = 'UPDATE' and old_id is distinct from new_id) then
        execute format('delete from %s where descendant = $1::%s or ancestor = $1::%s', c, typ, typ) using old_id;
    elsif tg_op = 'UPDATE' then
        -- a node moved below itself would make the tree a cycle, which no expansion can follow
        execute format('select exists (select 1 from %s where ancestor = $1::%s and descendant = $2::%s)', c, typ, typ)
            into cycle using new_id, new_parent;
        if cycle then
            raise exception 'moving % under % would make a cycle in %', new_id, new_parent, tg_table_name
                using errcode = 'integrity_constraint_violation';
        end if;
        -- a moved subtree: unlink it from the ancestors above it, link it under the new parent
        execute format('delete from %1$s where descendant in (select descendant from %1$s where ancestor = $1::%2$s)
                          and ancestor in (select ancestor from %1$s where descendant = $1::%2$s and ancestor <> $1::%2$s)',
                       c, typ) using new_id;
        if new_parent is not null then
            execute format('insert into %1$s (ancestor, descendant)
                            select a.ancestor, s.descendant from %1$s a, %1$s s
                            where a.descendant = $1::%2$s and s.ancestor = $2::%2$s
                            on conflict do nothing', c, typ) using new_parent, new_id;
        end if;
    end if;

    if tg_op = 'INSERT' or (tg_op = 'UPDATE' and old_id is distinct from new_id) then
        execute format('insert into %1$s (ancestor, descendant) values ($1::%2$s, $1::%2$s) on conflict do nothing', c, typ)
            using new_id;
        if new_parent is not null then
            execute format('insert into %1$s (ancestor, descendant)
                            select ancestor, $1::%2$s from %1$s where descendant = $2::%2$s
                            on conflict do nothing', c, typ) using new_id, new_parent;
        end if;
    end if;

    return null;
end
$$;

-- After each statement that changed the tree: bump the version (expansions cached across
-- transactions are dropped), and empty the closure on TRUNCATE. TG_ARGV: closure.
create function authz_internal.closure_changed() returns trigger
language plpgsql security definer set search_path = pg_catalog, pg_temp as $$
begin
    if tg_op = 'TRUNCATE' then
        execute format('truncate %s', tg_argv[0]);
    end if;
    update authz_internal.hierarchies set version = version + 1 where closure = tg_argv[0]::regclass;
    return null;
end
$$;

-- Declare that p_type#p_permission is inherited along p_source(p_id, p_parent), with
-- p_direct the relation holding grants made on a node itself. Builds the closure and installs
-- the triggers that maintain it.
create function authz_internal.declare_hierarchy(
    p_type text, p_permission text, p_direct text, p_source regclass, p_id name, p_parent name
) returns void
language plpgsql volatile as $$
declare
    typ text;
    c   text := format('authz.closure_%s', p_source::oid);
begin
    -- one level: the engine is asked for the direct permission, which must not be expanded itself
    if p_direct = p_permission or exists (
        select 1 from authz_internal.hierarchies h
        where h.object_type = p_type and h.permission <> p_permission and (h.permission = p_direct or h.direct_permission = p_permission)
    ) then
        raise exception '%#% cannot expand %#%, which is a declared hierarchy or expands it', p_type, p_permission, p_type, p_direct;
    end if;
    select format_type(a.atttypid, a.atttypmod) into typ
    from pg_attribute a where a.attrelid = p_source and a.attname = p_id and not a.attisdropped;
    if typ is null then
        raise exception 'column % does not exist on %', p_id, p_source;
    end if;
    if not exists (select 1 from pg_attribute where attrelid = p_source and attname = p_parent and not attisdropped) then
        raise exception 'column % does not exist on %', p_parent, p_source;
    end if;

    execute format('create table if not exists %s (ancestor %s not null, descendant %s not null, primary key (ancestor, descendant))', c, typ, typ);
    execute format('create index if not exists %I on %s (descendant)', format('closure_%s_descendant', p_source::oid), c);
    execute format('revoke all on %s from public', c);
    execute format('truncate %s', c);
    -- every node with each of its ancestors (itself included); UNION stops on a cycle
    execute format('insert into %1$s (ancestor, descendant)
                    with recursive up(node, ancestor) as (
                        select %2$I, %2$I from %3$s
                        union
                        select u.node, p.%4$I from up u join %3$s p on p.%2$I = u.ancestor where p.%4$I is not null)
                    select ancestor, node from up', c, p_id, p_source, p_parent);

    execute format('create or replace trigger authz_closure after insert or update of %I, %I or delete on %s
                    for each row execute function authz_internal.closure_maintain(%L, %L, %L, %L)',
                   p_id, p_parent, p_source, c, p_id, p_parent, typ);
    execute format('create or replace trigger authz_closure_changed after insert or update of %I, %I or delete or truncate on %s
                    for each statement execute function authz_internal.closure_changed(%L)',
                   p_id, p_parent, p_source, c);

    insert into authz_internal.hierarchies (object_type, permission, direct_permission, source, id_column, parent_column, closure)
    values (p_type, p_permission, p_direct, p_source, p_id, p_parent, c::regclass)
    on conflict (object_type, permission) do update
        set direct_permission = excluded.direct_permission, source = excluded.source, id_column = excluded.id_column,
            parent_column = excluded.parent_column, closure = excluded.closure, version = authz_internal.hierarchies.version + 1;
end
$$;

-- For the extension (as any role): p_type#p_permission's direct permission and the closure's
-- version, or no row when it is not a declared hierarchy.
create function authz.hierarchy_of(p_type text, p_permission text, out direct_permission text, out version bigint)
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select h.direct_permission, h.version from authz_internal.hierarchies h
    where h.object_type = p_type and h.permission = p_permission
$$;

-- For the extension (as any role): every declared hierarchy as (type, permission, direct
-- permission) triples, flattened. Backends keep it until authz_internal.hierarchies changes.
create function authz.hierarchy_declarations() returns text[]
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select coalesce(array_agg(u.v order by h.object_type, h.permission, u.n), '{}')
    from authz_internal.hierarchies h, unnest(array[h.object_type, h.permission, h.direct_permission]) with ordinality u(v, n)
$$;

-- The objects p_type#p_permission reaches for the current subject: its direct grants (asked of
-- the engine here, so a caller cannot expand nodes it was not granted) and everything below them.
create function authz.hierarchy_expand(p_type text, p_permission text) returns text[]
language plpgsql stable security definer set search_path = pg_catalog, pg_temp as $$
declare
    h     authz_internal.hierarchies;
    typ   text;
    out   text[];
    p_ids text[];
begin
    select * into h from authz_internal.hierarchies where object_type = p_type and permission = p_permission;
    if h is null then
        return null;
    end if;
    p_ids := authz.allowed_text(p_type, h.direct_permission, h.source::oid, h.id_column::text);
    select format_type(a.atttypid, a.atttypmod) into typ
    from pg_attribute a where a.attrelid = h.closure and a.attname = 'ancestor';
    execute format('select array(select distinct descendant::text from %s where ancestor = any($1::%s[])
                                 union select unnest($1))', h.closure, typ)
        into out using p_ids;
    return out;
end
$$;

-- Stop expanding p_type#p_permission in Postgres (the engine answers it again). The closure and
-- its trigger go with the last hierarchy using them.
create function authz_internal.drop_hierarchy(p_type text, p_permission text) returns void
language plpgsql volatile as $$
declare
    h authz_internal.hierarchies;
begin
    delete from authz_internal.hierarchies where object_type = p_type and permission = p_permission returning * into h;
    if h is null or exists (select 1 from authz_internal.hierarchies where closure = h.closure) then
        return;
    end if;
    execute format('drop trigger if exists authz_closure on %s', h.source);
    execute format('drop trigger if exists authz_closure_changed on %s', h.source);
    execute format('drop table if exists %s', h.closure);
end
$$;

-- Backends keep pg_auth's configuration across transactions: a change to it makes them reload
-- (a tree's version bump is not one; expansions check the version themselves).
create function authz_internal.config_changed() returns trigger
language plpgsql security definer set search_path = pg_catalog, pg_temp as $$
begin
    perform authz_internal.invalidate_config();
    return null;
end
$$;
create trigger authz_config_changed after insert or delete or truncate or update on authz_internal.settings
    for each statement execute function authz_internal.config_changed();
create trigger authz_config_changed after insert or delete or truncate
    or update of object_type, permission, direct_permission on authz_internal.hierarchies
    for each statement execute function authz_internal.config_changed();

