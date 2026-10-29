-- Friendly API:
--   authz.protect          protect a table with a permission of the engine's model
--   authz.check / can      ask the engine (any subject / the current subject)
--   authz.status           what pg_auth is enforcing, and whether the engine answers

-- Protect a table: each row is an object of the engine's `type`, its id in column `id` (the row's
-- own id, or a parent's such as org_id) or, with `key`, the value of an IMMUTABLE expression over
-- the row's columns (a storage path's first folder: split_part(name, '/', 1)). Reads need the
-- `read` permission; writes need `write` (denied when null).
--
--   select authz.protect('app.documents', type => 'document', id => 'id', read => 'viewer', write => 'editor');
--
-- A row may be reachable several ways (an order visible to its buyer, or to its seller's staff):
-- protect the table once per way, each with its own `name`; a row is visible when any allows it.
-- insert_type / insert_id / insert_permission check INSERT against another object, usually the
-- parent (a document may be created by whoever may create in its project), since a row's own
-- object does not exist in the engine until the row does.
--
-- hierarchy => 'app.folders(id, parent_id)' declares that type#read is inherited along that tree,
-- with `direct` (e.g. 'viewer_direct') the relation for grants made on a node itself: the model
-- must read `read: direct or read from parent`, its parent edges being that column. pg_auth then
-- asks the engine only for a subject's direct grants and expands them in Postgres.
--
-- attributes => array['owner_id', 'status'] sends those columns as the row's attributes, for an
-- engine that decides on them (Cerbos).
create function authz.protect(
    rel regclass,
    type text,
    id name,
    read text,
    write text default null,
    force boolean default true,
    key text default null,
    name text default null,
    insert_type text default null,
    insert_id name default null,
    insert_permission text default null,
    hierarchy text default null,
    direct text default null,
    attributes name[] default null
) returns void
language plpgsql volatile as $$
#variable_conflict use_column
declare
    tree text[];
begin
    if (protect.hierarchy is null) <> (protect.direct is null) then
        raise exception 'a hierarchy needs both hierarchy => ''table(id, parent)'' and direct => (the relation of direct grants)';
    end if;
    if protect.hierarchy is not null then
        tree := regexp_match(protect.hierarchy, '^\s*(.+?)\s*\(\s*(\w+)\s*,\s*(\w+)\s*\)\s*$');
        if tree is null then
            raise exception 'hierarchy must read ''table(id, parent)'', got %', protect.hierarchy;
        end if;
        perform authz.declare_hierarchy(protect.type, protect.read, protect.direct, tree[1]::regclass, tree[2]::name, tree[3]::name);
    end if;
    if (protect.id is null) = (protect.key is null) then
        raise exception 'protect % by a column (id => ...) or by a key expression (key => ...), not both or neither', protect.rel;
    end if;
    if (protect.insert_type is null) <> (protect.insert_id is null) or (protect.insert_id is null) <> (protect.insert_permission is null) then
        raise exception 'checking inserts against another object needs insert_type, insert_id and insert_permission';
    end if;
    perform authz.bind(protect.rel, protect.id, protect.type, protect.read, protect.write, protect.force, protect.key, protect.name,
                       protect.insert_type, protect.insert_id, protect.insert_permission, protect.attributes);
    raise notice '% is protected: a row is %:<%>; reading needs %, writing %', protect.rel, protect.type,
        coalesce(quote_ident(protect.id), protect.key), protect.read,
        coalesce('needs ' || protect.write, 'is denied (no write permission)')
        || coalesce('; decided on the row''s ' || array_to_string(protect.attributes, ', '), '');

    if protect.key is not null then
        if not exists (select 1 from pg_index i where i.indrelid = protect.rel and i.indexprs is not null
                       and pg_get_indexdef(i.indexrelid) like '%' || protect.key || '%') then
            raise notice 'no index on the key expression of %; pg_auth filters on it. Consider: create index concurrently on % ((%))',
                protect.rel, protect.rel, protect.key;
        end if;
    elsif not exists (select 1 from pg_index i
                      join pg_attribute a on a.attrelid = i.indrelid and a.attnum = i.indkey[0]
                      where i.indrelid = protect.rel and a.attname = protect.id) then
        raise notice 'no index starts with %.%; pg_auth filters on it. Consider: create index concurrently on % (%)',
            protect.rel, protect.id, protect.rel, quote_ident(protect.id);
    end if;
end
$$;

-- What pg_auth is enforcing, and whether the engine answers.
create function authz.status() returns table (item text, value text)
language sql stable as $$
    select 'engine', authz.engine()
    union all select 'sidecar socket', current_setting('pg_auth.sidecar_socket')
    union all select 'subject source', (select value from authz.settings where key = 'subject_source')
    union all
    select 'protected ' || b.rel::text || case when b.name = 'pg_auth' then '' else ' (' || b.name || ')' end,
           b.object_type || '#' || b.permission
           || coalesce(' / write ' || b.write_permission, '')
           || ' via ' || coalesce(quote_ident(b.column_name), '(' || b.key_expr || ')')
           || coalesce(' / insert ' || b.insert_object_type || '#' || b.insert_permission || ' via ' || quote_ident(b.insert_column), '')
           || case when b.force then '' else ' (owner not forced)' end
    from authz.bindings b
    union all
    select 'hierarchy ' || h.object_type || '#' || h.permission,
           h.object_type || '#' || h.direct_permission || ' down ' || h.source::text
           || '(' || quote_ident(h.id_column) || ', ' || quote_ident(h.parent_column) || ')'
           || ', version ' || h.version
    from authz.hierarchies h
$$;

-- Why a row is, or is not, visible: who pg_auth sees, the table's row level security, the row, and
-- each of pg_auth's policies on it evaluated for that subject, with the table's other policies
-- listed. `as_subject` acts as another subject ('user:anne') for the call, whatever the subject
-- source (pg_auth.as_subject), with `subject_roles` its roles for an engine that decides on them
-- (Cerbos); `as_role` also checks that Postgres role's privilege to read the table.
-- It reads rows the subject may not see, so it is for administrators.
--
--   select * from authz.why('app.documents', '42', as_subject => 'user:anne', as_role => 'authenticated');
create function authz.why(rel regclass, id text, as_subject text default null, as_role name default null,
                          subject_roles text[] default null)
returns table (step text, ok boolean, detail text)
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
#variable_conflict use_column
declare
    previous text := current_setting('pg_auth.as_subject', true);
    previous_roles text := current_setting('pg_auth.subject_roles', true);
    who record;
    pk name;
    pktype regtype;
    found boolean;
    b record;
    qual text;
    allowed boolean;
    object text;
    visible boolean := false;
    pol record;
    rls boolean;
    forced boolean;
    failure text;
begin
    if why.as_subject is not null then
        perform set_config('pg_auth.as_subject', why.as_subject, true);
    end if;
    if why.subject_roles is not null then
        perform set_config('pg_auth.subject_roles', array_to_string(why.subject_roles, ','), true);
    end if;
    select * into who from authz.current_subject();
    step := 'subject'; ok := who.subject_id is not null;
    detail := case when who.subject_id is null then 'none: ' || who.detail || '. No subject sees no rows'
                   else who.subject_type || ':' || who.subject_id || ' (' || who.detail || ')' end;
    return next;

    select c.relrowsecurity, c.relforcerowsecurity into rls, forced from pg_class c where c.oid = why.rel;
    step := 'row level security'; ok := rls;
    detail := case when not rls then 'off: every row is visible to whoever may read the table'
                   when forced then 'on, forced: the table owner is checked too'
                   else 'on, not forced: the table owner sees every row' end;
    return next;

    if why.as_role is not null then
        step := 'privilege'; ok := has_table_privilege(why.as_role, why.rel, 'SELECT');
        detail := why.as_role || case when ok then ' may read ' else ' may not read ' end || why.rel::text
                  || case when ok then '' else ': grant select on ' || why.rel::text || ' to ' || quote_ident(why.as_role) end;
        return next;
    end if;

    select a.attname, a.atttypid::regtype into pk, pktype
    from pg_index i join pg_attribute a on a.attrelid = i.indrelid and a.attnum = i.indkey[0]
    where i.indrelid = why.rel and i.indisprimary and i.indnkeyatts = 1;
    if pk is null then
        step := 'row'; ok := false; detail := why.rel::text || ' has no single-column primary key to find the row by';
        return next;
        perform set_config('pg_auth.as_subject', coalesce(previous, ''), true);
        perform set_config('pg_auth.subject_roles', coalesce(previous_roles, ''), true);
        return;
    end if;
    execute format('select exists (select 1 from %s where %I = $1::%s)', why.rel, pk, pktype) using why.id into found;
    step := 'row'; ok := found;
    detail := case when found then format('%s = %s exists', pk, why.id) else format('no row with %s = %s', pk, why.id) end;
    return next;

    if found then
        for b in select * from authz.bindings where bindings.rel = why.rel order by bindings.name loop
            select pg_get_expr(p.polqual, p.polrelid) into qual from pg_policy p
            where p.polrelid = why.rel and p.polname = b.name || '_select';
            failure := null;
            begin
                execute format('select (%s)::text, %s from %s where %I = $1::%s',
                               coalesce(quote_ident(b.column_name), b.key_expr), coalesce(qual, 'false'), why.rel, pk, pktype)
                    using why.id into object, allowed;
            exception when others then
                -- an engine that cannot answer for this subject: say so, and go on explaining
                failure := sqlerrm;
                allowed := false;
                execute format('select (%s)::text from %s where %I = $1::%s',
                               coalesce(quote_ident(b.column_name), b.key_expr), why.rel, pk, pktype) using why.id into object;
            end;
            object := b.object_type || ':' || coalesce(object, 'null');
            step := 'policy ' || b.name; ok := coalesce(allowed, false);
            detail := case when failure is not null then format('%s on %s could not be decided: %s', b.permission, object, failure)
                           when object like '%:null' then 'the row''s ' || coalesce(b.column_name, 'key') || ' is NULL: no object, never visible'
                           when b.attributes is not null then format('%s on %s with the row''s %s: %s', b.permission, object,
                                array_to_string(b.attributes, ', '), case when ok then 'allowed' else 'denied by the engine''s rules' end)
                           else format('%s %s %s on %s', coalesce(who.subject_type || ':' || who.subject_id, 'no subject'),
                                case when ok then 'has' else 'lacks' end, b.object_type || '#' || b.permission, object) end
                      || case when b.write_permission is null then '; writes are denied (no write permission)'
                              else '; writes need ' || b.object_type || '#' || b.write_permission end;
            visible := visible or ok;
            return next;
        end loop;
        if not exists (select 1 from authz.bindings where bindings.rel = why.rel) then
            step := 'pg_auth'; ok := null; detail := why.rel::text || ' is not protected by pg_auth: authz.protect(...)';
            return next;
        end if;
    end if;

    for pol in select p.polname, p.polpermissive, p.polcmd, pg_get_expr(p.polqual, p.polrelid) as using_expr
               from pg_policy p where p.polrelid = why.rel
               and not exists (select 1 from authz.bindings bb where bb.rel = why.rel
                               and p.polname in (bb.name || '_select', bb.name || '_insert', bb.name || '_update', bb.name || '_delete'))
               and p.polcmd in ('r', '*') loop
        step := 'other policy ' || pol.polname; ok := null;
        detail := case when pol.polpermissive then 'permissive (ORed with pg_auth''s): ' else 'restrictive (ANDed with pg_auth''s): ' end
                  || coalesce(pol.using_expr, 'true') || ' (not evaluated here)';
        return next;
    end loop;

    step := 'verdict';
    ok := found and visible and coalesce(rls, false);
    detail := case when not found then 'hidden: the row does not exist'
                   when not coalesce(rls, false) then 'visible: row level security is off'
                   when who.subject_id is null then 'hidden: no subject'
                   when visible then 'visible to ' || who.subject_type || ':' || who.subject_id || ', unless a restrictive policy above says otherwise'
                   else 'hidden from ' || who.subject_type || ':' || who.subject_id || ': no pg_auth policy allows it' end;
    return next;
    perform set_config('pg_auth.as_subject', coalesce(previous, ''), true);
    perform set_config('pg_auth.subject_roles', coalesce(previous_roles, ''), true);
end
$$;

-- Check the setup and say what to fix: one row per check, with its status (ok, warn, fail, info)
-- and, where something is off, the statement that fixes it.
--
--   select * from authz.doctor() where status <> 'ok';
create function authz.doctor()
returns table (check_name text, status text, detail text, fix text)
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
declare
    engine text;
    who record;
    b record;
    t record;
    readers text;
    api_roles name[] := array(select rolname from pg_roles where rolname in ('anon', 'authenticated', 'web_anon', 'authenticator'));
begin
    check_name := 'preloaded';
    if current_setting('shared_preload_libraries') ~ '(^|[ ,])"?pg_auth"?([ ,]|$)' then
        status := 'ok'; detail := 'pg_auth is in shared_preload_libraries'; fix := null;
    else
        status := 'fail'; detail := 'pg_auth is not preloaded: its planner hook only applies once a session has called one of its functions';
        fix := 'alter system set shared_preload_libraries = ''pg_auth''; -- then restart Postgres';
    end if;
    return next;

    check_name := 'sidecar';
    if coalesce(current_setting('pg_auth.sidecar_socket', true), '') = '' then
        status := 'fail'; detail := 'no sidecar socket configured: every protected read fails closed';
        fix := 'alter system set pg_auth.sidecar_socket = ''/run/pg_auth/default.sock''; select pg_reload_conf();';
    else
        engine := authz.engine();
        if engine like 'unreachable%' then
            status := 'fail'; detail := current_setting('pg_auth.sidecar_socket') || ': ' || engine;
            fix := 'start the sidecar (sidecar/query) on that socket, readable by the postgres OS user';
        else
            status := 'ok'; detail := 'the sidecar answers at ' || current_setting('pg_auth.sidecar_socket') || ', engine ' || engine; fix := null;
        end if;
    end if;
    return next;

    check_name := 'subject source';
    status := 'ok'; fix := null;
    detail := (select string_agg(key || ' = ' || value, ', ' order by key) from authz.settings
               where key in ('subject_source', 'claims_setting', 'claim', 'subject_type', 'roles_claim'));
    if (select value from authz.settings where key = 'subject_source') = 'jwt' then
        status := 'info';
        detail := detail || '. The claims are trusted as they are: only a trusted API (PostgREST) may set them';
        fix := 'select authz.configure(subject => ''jwt_signed'', jwks => ...); -- to verify the token instead';
    end if;
    return next;

    select * into who from authz.current_subject();
    check_name := 'this session''s subject'; fix := null;
    status := case when who.subject_id is null then 'info' else 'ok' end;
    detail := case when who.subject_id is null then 'none (' || who.detail || '): this session sees no protected rows'
                   else who.subject_type || ':' || who.subject_id || ' (' || who.source || ')' end;
    return next;

    if not exists (select 1 from authz.bindings) then
        check_name := 'protected tables'; status := 'info'; detail := 'no table is protected yet';
        fix := 'select authz.protect(''app.documents'', type => ''document'', id => ''id'', read => ''viewer'', write => ''editor'');';
        return next;
    end if;
    for b in select bb.*, c.relrowsecurity, c.relforcerowsecurity from authz.bindings bb join pg_class c on c.oid = bb.rel order by bb.rel::text, bb.name loop
        check_name := 'protected ' || b.rel::text || case when b.name = 'pg_auth' then '' else ' (' || b.name || ')' end;
        if not b.relrowsecurity then
            status := 'fail'; detail := 'row level security is off: every row is visible';
            fix := format('alter table %s enable row level security;', b.rel);
        elsif b.force and not b.relforcerowsecurity then
            status := 'fail'; detail := 'row level security is not forced: the table owner (and its views) sees every row';
            fix := format('alter table %s force row level security;', b.rel);
        elsif b.column_name is not null and not exists (
                select 1 from pg_index i join pg_attribute a on a.attrelid = i.indrelid and a.attnum = i.indkey[0]
                where i.indrelid = b.rel and a.attname = b.column_name) then
            status := 'warn'; detail := format('%s#%s via %I, which no index starts with: pg_auth filters on it', b.object_type, b.permission, b.column_name);
            fix := format('create index concurrently on %s (%I);', b.rel, b.column_name);
        else
            readers := (select string_agg(r, ', ') from unnest(api_roles) r where has_table_privilege(r, b.rel, 'SELECT'));
            status := 'ok'; fix := null;
            detail := format('%s#%s via %s', b.object_type, b.permission, coalesce(quote_ident(b.column_name), '(' || b.key_expr || ')'))
                      || coalesce('; writes need ' || b.write_permission, '; writes denied')
                      || coalesce('; readable by ' || readers, '');
        end if;
        return next;
    end loop;

    -- relationship tables: changes waiting for the sidecar, and changes the engine refused
    if exists (select 1 from authz.relationships) then
        check_name := 'relationship tables';
        select count(*) filter (where error is not null) as refused, min(at) filter (where error is null) as oldest
        into t from authz.outbox;
        if t.refused > 0 then
            status := 'fail';
            detail := t.refused || ' changes the engine refused, e.g. ' || (select error from authz.outbox where error is not null order by id limit 1);
            fix := 'select * from authz.outbox where error is not null;  -- fix the model or the rows, then delete these';
        elsif t.oldest < now() - interval '1 minute' then
            status := 'warn';
            detail := 'changes waiting since ' || t.oldest || ': is a sidecar with PG_AUTH_DATABASE_URL running?';
            fix := 'start the sidecar with PG_AUTH_DATABASE_URL and grant it authz.outbox_claim, outbox_done, outbox_failed, answers_changed';
        else
            status := 'ok'; detail := (select count(*) from authz.relationships) || ' declared, delivered'; fix := null;
        end if;
        return next;
    end if;

    -- tables an API role can read with no row level security at all
    for t in select c.oid::regclass as rel
             from pg_class c join pg_namespace n on n.oid = c.relnamespace
             where c.relkind in ('r', 'p') and not c.relrowsecurity
               and n.nspname not in ('pg_catalog', 'information_schema', 'authz') and n.nspname not like 'pg\_%'
               and exists (select 1 from unnest(api_roles) r where has_table_privilege(r, c.oid, 'SELECT'))
             order by 1 loop
        check_name := 'exposed ' || t.rel::text;
        status := 'warn';
        detail := 'readable by ' || (select string_agg(r, ', ') from unnest(api_roles) r where has_table_privilege(r, t.rel, 'SELECT'))
                  || ' with no row level security: every row is public to them';
        fix := format('select authz.protect(%L, type => ..., id => ..., read => ...);  -- or revoke select', t.rel::text);
        return next;
    end loop;
end
$$;

revoke execute on function authz.protect(regclass, text, name, text, text, boolean, text, text, text, name, text, text, text, name[]) from public;
revoke execute on function authz.check(text, text, text) from public;
revoke execute on function authz.status() from public;
revoke execute on function authz.why(regclass, text, text, name, text[]) from public;
revoke execute on function authz.doctor() from public;
revoke execute on function authz.answers_changed() from public;
