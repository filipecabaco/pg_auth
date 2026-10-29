-- The whole setup as one document: who is asking, which tables are relationships, and how each
-- table is protected, applied in one transaction and kept, with every earlier version, for
-- review. It compiles into authz.configure, authz.relate and authz.protect.
--
--   select authz.setup($${
--     "subject": {"source": "jwt", "claim": "sub"},
--     "relationships": [
--       {"table": "app.shares", "by": "grantee_kind",
--        "object": {"type": "doc", "column": "doc_id"},
--        "relation": {"column": "role", "allowed": ["viewer", "editor"]},
--        "subject": {"user": {"type": "user", "column": "grantee_id"},
--                    "team": {"type": "team", "column": "grantee_id", "relation": "member"},
--                    "public": {"type": "user", "id": "*"}}}],
--     "protect": [
--       {"table": "app.docs", "type": "doc", "column": "id",
--        "select": "viewer", "write": "editor", "delete": "owner"}]
--   }$$);
--
-- "column" names a column of the row, "id" a fixed value ('*': everyone). Anything the previous
-- document declared and this one does not is removed. select authz.setup() returns the document
-- in force; authz.setups keeps every version.

create table authz.setups (
    version      bigserial primary key,
    applied_at   timestamptz not null default now(),
    applied_by   name not null default current_user,
    document     jsonb not null,
    -- what it declared, as 'table/name', so the next document can remove what it drops
    relationships text[] not null,
    protections  text[] not null
);

-- Unknown keys are typos: fail rather than ignore them.
create function authz.setup_keys(obj jsonb, allowed text[], what text) returns void
language plpgsql immutable as $$
declare
    k text;
begin
    if jsonb_typeof(obj) is distinct from 'object' then
        raise exception 'setup: % must be an object, got %', what, coalesce(obj::text, 'nothing');
    end if;
    for k in select jsonb_object_keys(obj) loop
        if not k = any (allowed) then
            raise exception 'setup: % has no key "%" (it takes %)', what, k, array_to_string(allowed, ', ');
        end if;
    end loop;
end
$$;

-- One relationship declaration, unless the same one is already in force (relate would deliver the
-- table's rows again).
create function authz.setup_relate(rel regclass, decl_name text, filter text, object jsonb, relation jsonb, subject jsonb, what text,
                                   expires jsonb default null, condition jsonb default null)
returns void
language plpgsql volatile as $$
declare
    want authz.relationships;
    have authz.relationships;
begin
    perform authz.setup_keys(object, array['type', 'type_column', 'column', 'id'], what || ' object');
    perform authz.setup_keys(coalesce(expires, '{}'), array['column'], what || ' expires');
    perform authz.setup_keys(coalesce(condition, '{}'), array['name', 'context'], what || ' condition');
    perform authz.setup_keys(subject, array['type', 'type_column', 'column', 'id', 'relation', 'relation_column'], what || ' subject');
    if jsonb_typeof(relation) = 'object' then
        perform authz.setup_keys(relation, array['column', 'allowed'], what || ' relation');
    elsif jsonb_typeof(relation) is distinct from 'string' then
        raise exception 'setup: % relation is a name, or {"column": ..., "allowed": [...]}', what;
    end if;
    want := (rel, decl_name, object ->> 'type', object ->> 'type_column', object ->> 'id', object ->> 'column',
             case when jsonb_typeof(relation) = 'string' then relation #>> '{}' end, relation ->> 'column',
             subject ->> 'type', subject ->> 'type_column', subject ->> 'id', subject ->> 'column',
             subject ->> 'relation', subject ->> 'relation_column', filter,
             (select array_agg(r) from jsonb_array_elements_text(relation -> 'allowed') r), expires ->> 'column',
             condition ->> 'name', condition -> 'context');
    select * into have from authz.relationships r where r.rel = setup_relate.rel and r.name = decl_name;
    if have is not distinct from want then
        return;
    end if;
    perform authz.relate(want.rel, name => want.name, filter => want.filter,
                         object_type => want.object_type, object_type_column => want.object_type_column,
                         object_column => want.object_column, object_id => want.object_id,
                         relation => want.relation, relation_column => want.relation_column, relations => want.relations,
                         subject_type => want.subject_type, subject_type_column => want.subject_type_column,
                         subject_column => want.subject_column, subject_id => want.subject_id,
                         subject_relation => want.subject_relation, subject_relation_column => want.subject_relation_column,
                         expires_column => want.expires_column, condition => want.condition, condition_context => want.condition_context);
end
$$;

-- Remove one of a table's protections, leaving its others (unbind removes them all).
create function authz.setup_unprotect(rel regclass, binding name) returns void
language plpgsql volatile as $$
declare
    part regclass;
    pol name;
begin
    if not exists (select 1 from authz.bindings b where b.rel = setup_unprotect.rel and b.name <> binding) then
        perform authz.unbind(rel);
        return;
    end if;
    for part in select setup_unprotect.rel union select relid from pg_partition_tree(setup_unprotect.rel) loop
        for pol in select polname from pg_policy where polrelid = part
                   and polname in (binding || '_select', binding || '_insert', binding || '_update', binding || '_delete') loop
            execute format('drop policy %I on %s', pol, part);
        end loop;
    end loop;
    delete from authz.bindings b where b.rel = setup_unprotect.rel and b.name = binding;
end
$$;

create function authz.setup(document jsonb) returns bigint
language plpgsql volatile as $$
declare
    s jsonb := document -> 'subject';
    e jsonb;
    v record;
    rel regclass;
    base text;
    relationships text[] := '{}';
    protections text[] := '{}';
    previous authz.setups;
    gone text;
    insert_ jsonb;
    attrs jsonb;
    applied bigint;
    what text;
begin
    perform authz.setup_keys(document, array['subject', 'relationships', 'protect'], 'the document');

    if s is not null then
        perform authz.setup_keys(s, array['source', 'type', 'claim', 'jwks', 'claims_setting', 'token_setting', 'token_header',
                                          'clock_skew_seconds', 'roles', 'scope', 'policy_version', 'context'], 'subject');
        perform authz.configure(subject => s ->> 'source', subject_type => s ->> 'type', claim => s ->> 'claim',
                                jwks => s -> 'jwks', claims_setting => s ->> 'claims_setting',
                                token_setting => s ->> 'token_setting', token_header => s ->> 'token_header',
                                clock_skew_seconds => (s ->> 'clock_skew_seconds')::int, roles_claim => s ->> 'roles',
                                scope_claim => s ->> 'scope', policy_version => s ->> 'policy_version', context => s -> 'context');
    end if;

    for e in select x from jsonb_array_elements(coalesce(document -> 'relationships', '[]')) x loop
        perform authz.setup_keys(e, array['table', 'name', 'filter', 'by', 'object', 'relation', 'subject', 'expires', 'condition'], 'a relationship');
        rel := (e ->> 'table')::regclass;
        base := coalesce(e ->> 'name', 'pg_auth');
        what := format('relationship %s (%s)', rel, base);
        if e ? 'by' then
            -- one declaration per kind: the subject is a map from the by column's values
            for v in select key, value from jsonb_each(e -> 'subject') loop
                perform authz.setup_relate(rel, base || '_' || v.key,
                    concat_ws(' and ', '(' || (e ->> 'filter') || ')', format('%I = %L', e ->> 'by', v.key)),
                    e -> 'object', e -> 'relation', v.value, what || ' ' || v.key, e -> 'expires', e -> 'condition');
                relationships := relationships || (rel::text || '/' || base || '_' || v.key);
            end loop;
        else
            perform authz.setup_relate(rel, base, e ->> 'filter', e -> 'object', e -> 'relation', e -> 'subject', what, e -> 'expires', e -> 'condition');
            relationships := relationships || (rel::text || '/' || base);
        end if;
    end loop;

    for e in select x from jsonb_array_elements(coalesce(document -> 'protect', '[]')) x loop
        perform authz.setup_keys(e, array['table', 'name', 'type', 'column', 'key', 'select', 'write', 'insert', 'update',
                                          'delete', 'attributes', 'hierarchy', 'force'], 'a protection');
        rel := (e ->> 'table')::regclass;
        insert_ := e -> 'insert';
        attrs := e -> 'attributes';
        if jsonb_typeof(insert_) = 'object' then
            perform authz.setup_keys(insert_, array['type', 'column', 'permission'], format('protection %s insert', rel));
        end if;
        if e ? 'hierarchy' then
            perform authz.setup_keys(e -> 'hierarchy', array['tree', 'direct'], format('protection %s hierarchy', rel));
        end if;
        perform authz.protect(rel, type => e ->> 'type', id => e ->> 'column', key => e ->> 'key', read => e ->> 'select',
            -- "insert" as a name is the write permission inserts need; as an object, a parent's
            write => coalesce(e ->> 'write', case when jsonb_typeof(insert_) = 'string' then insert_ #>> '{}' end),
            on_update => e ->> 'update', on_delete => e ->> 'delete',
            insert_type => insert_ ->> 'type', insert_id => insert_ ->> 'column', insert_permission => insert_ ->> 'permission',
            attributes => case when jsonb_typeof(attrs) = 'array' then (select array_agg(a::name) from jsonb_array_elements_text(attrs) a) end,
            attribute_map => case when jsonb_typeof(attrs) = 'object' then attrs end,
            hierarchy => e -> 'hierarchy' ->> 'tree', direct => e -> 'hierarchy' ->> 'direct',
            force => coalesce((e ->> 'force')::boolean, true), name => e ->> 'name');
        protections := protections || (rel::text || '/' || coalesce('pg_auth_' || (e ->> 'name'), 'pg_auth'));
    end loop;

    -- what the previous document declared and this one does not
    select * into previous from authz.setups order by version desc limit 1;
    if previous is not null then
        foreach gone in array previous.relationships loop
            if not gone = any (relationships) then
                perform authz.unrelate(split_part(gone, '/', 1)::regclass, split_part(gone, '/', 2));
            end if;
        end loop;
        foreach gone in array previous.protections loop
            if not gone = any (protections) and to_regclass(split_part(gone, '/', 1)) is not null then
                perform authz.setup_unprotect(split_part(gone, '/', 1)::regclass, split_part(gone, '/', 2));
            end if;
        end loop;
    end if;

    insert into authz.setups (document, relationships, protections) values (document, relationships, protections)
    returning setups.version into applied;
    raise notice 'setup % applied: % relationship declarations, % protections', applied,
        coalesce(array_length(relationships, 1), 0), coalesce(array_length(protections, 1), 0);
    return applied;
end
$$;

-- The document in force (null before the first).
create function authz.setup() returns jsonb
language sql stable as $$
    select document from authz.setups order by version desc limit 1
$$;

revoke all on authz.setups from public;
revoke execute on function authz.setup(jsonb) from public;
revoke execute on function authz.setup() from public;
revoke execute on function authz.setup_keys(jsonb, text[], text) from public;
revoke execute on function authz.setup_relate(regclass, text, text, jsonb, jsonb, jsonb, text, jsonb, jsonb) from public;
revoke execute on function authz.setup_unprotect(regclass, name) from public;
