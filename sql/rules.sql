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
    -- the engine's own id for it once pushed (OpenFGA's authorization model id), which every
    -- sidecar then asks with
    engine_ref text,
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

-- For the sidecar: the latest version of each of `p_engine`'s rules not pushed yet, with the one
-- the engine runs now (`previous`: Cerbos stores policies it cannot compile, and the sidecar puts
-- that one back), to the one sidecar pushing (an advisory lock held until the transaction ends).
create function authz.rules_pending(p_engine text) returns table (name text, version int, body text, previous text)
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
begin
    if not pg_try_advisory_xact_lock(hashtext('pg_auth.rules')) then
        return;
    end if;
    return query
    select l.name, l.version, l.body,
           (select r.body from authz.rules r
            where r.engine = p_engine and r.name = l.name and r.pushed_at is not null order by r.version desc limit 1)
    from authz.rules(p_engine) l where l.pushed_at is null and l.error is null;
end
$$;

-- For the sidecar: the engine took this version (no error; `p_ref` its id for it, if any), or
-- refused it for good (the error). The other sidecars hear of it, to ask with it too.
create function authz.rules_pushed(p_engine text, p_name text, p_version int, p_error text default null, p_ref text default null)
returns void
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    update authz.rules set pushed_at = case when p_error is null then now() end, error = p_error, engine_ref = p_ref
    where engine = p_engine and name = p_name and version = p_version;
    select pg_notify('pg_auth_rules', p_engine);
$$;

-- For the sidecar: the engine's id for the rules in force (OpenFGA's model id), or null.
create function authz.rules_ref(p_engine text) returns text
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select engine_ref from authz.rules where engine = p_engine and pushed_at is not null and engine_ref is not null
    order by pushed_at desc limit 1
$$;

revoke all on authz.rules from public;
revoke execute on function authz.set_rules(text, text, text) from public;
revoke execute on function authz.rules(text) from public;
revoke execute on function authz.rules_pending(text) from public;
revoke execute on function authz.rules_pushed(text, text, int, text, text) from public;
revoke execute on function authz.rules_ref(text) from public;

-- What the setup names that the stored rules do not have: a protected table's type, permission or
-- action; a relationship table's type, a relation that is computed rather than written, a subject
-- its relation does not take. Checked against the latest version stored, pushed or not, when the
-- rules of one engine are stored (several: which one a table is asked of is the sidecar's to say).
-- Parts read from a row's columns are the write's to check (`relations => array[...]`).
create function authz.rules_problems() returns table (item text, problem text)
language plpgsql stable as $$
declare
    eng text;
    rules text;
    names jsonb;
begin
    select min(r.engine) into eng from authz.rules() r having count(distinct r.engine) = 1;
    if eng is null then
        return;
    end if;
    rules := case eng when 'openfga' then 'the openfga model' when 'spicedb' then 'the spicedb schema' else 'the cerbos policies' end;
    select jsonb_agg(to_jsonb(n)) into names from authz.rules() r, authz.rules_names(r.engine, r.body) n;
    if jsonb_path_exists(names, '$[*] ? (@.kind == "unreadable")') then
        return;  -- the engine says what is wrong with them (authz.doctor)
    end if;

    return query
    with n as materialized (
        select * from jsonb_to_recordset(names) as x(object_type text, name text, kind text, subjects text[])
    ), uses as (
        -- protected tables: each permission (Cerbos: action) on the row's type, and on the parent's for inserts
        select b.rel::text || case when b.name <> 'pg_auth' then ' (' || b.name || ')' else '' end as item, u.t, u.p
        from authz.bindings b,
             lateral (values (b.object_type, null), (b.object_type, b.permission), (b.object_type, b.write_permission),
                             (b.object_type, b.update_permission), (b.object_type, b.delete_permission),
                             (b.insert_object_type, null), (b.insert_object_type, b.insert_permission)) u(t, p)
        where u.t is not null
    ), decl as (
        select r.rel::text || case when r.name <> 'pg_auth' then ' (' || r.name || ')' else '' end as item, r.*
        from authz.relationships r
        where eng <> 'cerbos'
    ), types as (
        -- relationship tables: their types, and each relation a row may write with the subject it names
        select d.item, t from decl d, lateral (values (d.object_type), (d.subject_type)) v(t) where t is not null
    ), writes as (
        -- each relationship a row writes: its relation, and the subject it names when that is fixed
        select d.item, d.object_type as t, u.wr as w, d.expires_column, d.condition,
               case when d.subject_type is not null and d.subject_relation_column is null
                    then d.subject_type || case when d.subject_id = '*' then ':*' else '' end || coalesce('#' || d.subject_relation, '') end as subject,
               eng = 'spicedb' and d.expires_column is not null as expiring,
               not (eng = 'spicedb' and d.expires_column is not null
                    and (select a.attnotnull from pg_attribute a where a.attrelid = d.rel and a.attname = d.expires_column)) as unending
        from decl d, unnest(case when d.relation is not null then array[d.relation] else d.relations end) as u(wr)
        where d.object_type is not null and exists (select 1 from n where n.object_type = d.object_type)
    ), forms as (
        -- the forms of subject those relationships are written in: with their condition, and on
        -- SpiceDB with an expiration when the row has an end (both when it may have none)
        select w.*, f.form, f.why
        from writes w,
             lateral (values
                 (case when w.unending then w.subject || coalesce(' with ' || w.condition, '') end,
                  case when w.expiring then format(': rows with no end (%s may be null) are written so', w.expires_column) else '' end),
                 (case when w.expiring then w.subject || ' with ' || coalesce(w.condition || ' and ', '') || 'expiration' end,
                  ': rows with an end are written so (and `use expiration`)')) f(form, why)
        where w.subject is not null and f.form is not null
    ), lists as (
        select n.object_type, string_agg(distinct n.name, ', ') filter (where n.kind <> 'type') as defined,
               string_agg(distinct n.name, ', ') filter (where n.kind = 'relation') as relations
        from n group by n.object_type
    ), all_types as (
        select string_agg(distinct n.object_type, ', ') filter (where n.kind = 'type') as types from n
    )
    select distinct u.item,
           case when u.p is null and eng = 'cerbos' then format('%s have no resource policy for %s', rules, u.t)
                when u.p is null then format('%s has no type %s (its types: %s)', rules, u.t, (select types from all_types))
                when eng = 'cerbos' then format('the cerbos policies for %s have no action %s (they have: %s)', u.t, u.p,
                                                coalesce((select defined from lists l where l.object_type = u.t), 'none'))
                else format('%s has no %s in %s (it has: %s)', u.t, u.p, rules, coalesce((select defined from lists l where l.object_type = u.t), 'none')) end
    from uses u
    where case when u.p is null then not exists (select 1 from n where n.object_type = u.t)
               else exists (select 1 from n where n.object_type = u.t)
                    and not exists (select 1 from n
                                    where n.object_type = u.t and n.kind <> 'type'
                                      and (n.name = u.p or (eng = 'cerbos' and (n.name = '*' or (n.name like '%:*' and starts_with(u.p, left(n.name, -1))))))) end
    union
    select t.item, format('%s has no type %s (its types: %s)', rules, t.t, (select types from all_types))
    from types t
    where not exists (select 1 from n where n.object_type = t.t)
    union
    select w.item,
           case when x.name is null then format('%s has no relation %s in %s (it has: %s)', w.t, w.w, rules,
                                                coalesce((select relations from lists l where l.object_type = w.t), 'none'))
                else format('%s#%s is computed in %s: rows write the relations it assigns (%s)', w.t, w.w, rules,
                            coalesce((select relations from lists l where l.object_type = w.t), 'none')) end
    from writes w
    left join n x on x.object_type = w.t and x.name = w.w and x.kind in ('relation', 'permission')
    where x.name is null or x.kind = 'permission'
    union
    select f.item, format('%s#%s does not take `%s` in %s (it takes: %s)%s', f.t, f.w, f.form, rules,
                          coalesce(nullif(array_to_string(x.subjects, ', '), ''), 'none'), f.why)
    from forms f
    join n x on x.object_type = f.t and x.name = f.w and x.kind = 'relation'
    where not f.form = any(x.subjects)
    union
    -- a condition the rules have, and the values a row gives it for its parameters
    select d.item, format('%s has no %s %s (it has: %s)', rules, case eng when 'openfga' then 'condition' else 'caveat' end, d.condition,
                          coalesce((select string_agg(c.name, ', ' order by c.name) from n c where c.kind = 'condition'), 'none'))
    from decl d
    where d.condition is not null and not exists (select 1 from n c where c.kind = 'condition' and c.name = d.condition)
    union
    select d.item, format('%s %s has no parameter %s (it has: %s)', case eng when 'openfga' then 'condition' else 'caveat' end,
                          d.condition, p.key, array_to_string(c.subjects, ', '))
    from decl d
    join n c on c.kind = 'condition' and c.name = d.condition,
         jsonb_object_keys(d.condition_context) p(key)
    where not p.key = any(c.subjects);
end
$$;

-- At commit: the setup and the rules change together in a migration, and are checked as one.
create function authz.rules_check() returns trigger
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
declare
    problems text;
begin
    select string_agg(p.item || ': ' || p.problem, E'\n' order by p.item, p.problem) into problems from authz.rules_problems() p;
    if problems is not null then
        raise exception 'the setup names what the rules stored with authz.set_rules do not have:%', E'\n' || problems
            using errcode = 'check_violation',
                  hint = 'correct the setup, or store rules that have them: both are checked when the transaction commits, so they can change together';
    end if;
    return null;
end
$$;

create constraint trigger rules_match after insert or update on authz.bindings
    deferrable initially deferred for each row execute function authz.rules_check();
create constraint trigger rules_match after insert or update on authz.relationships
    deferrable initially deferred for each row execute function authz.rules_check();
create constraint trigger rules_match after insert on authz.rules
    deferrable initially deferred for each row execute function authz.rules_check();

revoke execute on function authz.rules_problems() from public;
revoke execute on function authz.rules_check() from public;
