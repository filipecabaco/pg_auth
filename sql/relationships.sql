-- Relationship tables: rows an application writes (a membership, a share, a team) that are
-- relationships in the engine (OpenFGA, SpiceDB). An app with no backend grants and revokes
-- access by writing rows, through PostgREST like any other table; pg_auth delivers each change to
-- the engine.
--
-- A row trigger records each change in authz.outbox, in the writing transaction: a rolled back
-- write never reaches the engine, a committed one always does. The sidecar delivers the outbox in
-- order (authz.outbox_claim / authz.outbox_done, one sidecar at a time), retries what failed for
-- a while (the engine unreachable), and sets aside what the engine refused for good
-- (authz.outbox_failed); writes and deletes are idempotent in the engine, so a retried batch is
-- harmless. After each batch it calls authz.answers_changed(): every backend forgets the answers
-- it kept, and reads the engine at least as fresh as the write.

create table authz.relationships (
    rel              regclass not null,
    -- a table may describe several relationships (a share row: its user and its team)
    name             name not null default 'pg_auth',
    -- each part is fixed, or read from a column of the row: the object's type and id, the relation,
    -- the subject's type and id, and the subject's relation (a set of subjects: group:eng#member)
    object_type             text collate "C",
    object_type_column      name,
    object_id               text collate "C",
    object_column           name,
    relation                text collate "C",
    relation_column         name,
    subject_type            text collate "C",
    subject_type_column     name,
    -- a fixed subject id: '*' is everyone (user:*), a singleton (platform:main)
    subject_id              text collate "C",
    subject_column          name,
    subject_relation        text collate "C",
    subject_relation_column name,
    -- only rows this boolean SQL expression holds for (one shares table feeding a declaration per kind)
    filter                  text,
    -- the relations a row may name; another one fails the write, at once
    relations               text[],
    check ((object_type is null) <> (object_type_column is null)),
    check ((object_id is null) <> (object_column is null)),
    check ((relation is null) <> (relation_column is null)),
    check ((subject_type is null) <> (subject_type_column is null)),
    check ((subject_id is null) <> (subject_column is null)),
    check (subject_relation is null or subject_relation_column is null),
    primary key (rel, name)
);

create table authz.outbox (
    id       bigserial primary key,
    op       text not null check (op in ('write', 'delete')),
    object   text collate "C" not null,
    relation text collate "C" not null,
    subject  text collate "C" not null,
    at       timestamptz not null default now(),
    -- why the engine refused it, for good (an invalid relation, an unknown type): set aside, the
    -- changes after it delivered; authz.doctor() reports it, and deleting the row drops it
    error    text
);

-- The relationship a row of `r`'s table describes (object, relation, subject), or null when one
-- of its parts is null.
create function authz.relationship_of(r authz.relationships, row_ jsonb) returns text[]
language sql immutable as $$
    select case when p.ot is null or p.oid is null or p.rel is null or p.st is null or p.sid is null then null
                else array[p.ot || ':' || p.oid, p.rel, p.st || ':' || p.sid || coalesce('#' || p.sr, '')]
           end
    from (select coalesce(r.object_type, row_ ->> r.object_type_column) as ot,
                 coalesce(r.object_id, row_ ->> r.object_column) as oid,
                 coalesce(r.relation, row_ ->> r.relation_column) as rel,
                 coalesce(r.subject_type, row_ ->> r.subject_type_column) as st,
                 coalesce(r.subject_id, row_ ->> r.subject_column) as sid,
                 coalesce(r.subject_relation, row_ ->> r.subject_relation_column) as sr) p
$$;

-- Whether a row passes the declaration's filter (no filter: every row).
create function authz.relationship_applies(r authz.relationships, row_ anyelement) returns boolean
language plpgsql stable as $$
declare
    ok boolean;
begin
    if r.filter is null then
        return true;
    end if;
    execute format('select (%s) from (select ($1).*) as _', r.filter) into ok using row_;
    return coalesce(ok, false);
end
$$;

-- The row trigger of a relationship table (its argument: the declaration's name).
create function authz.relationship_changed() returns trigger
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
declare
    r authz.relationships;
    before text[];
    after text[];
begin
    select * into r from authz.relationships where rel = tg_relid and name = tg_argv[0];
    if r is null then
        return null;
    end if;
    if tg_op in ('UPDATE', 'DELETE') and authz.relationship_applies(r, old) then
        before := authz.relationship_of(r, to_jsonb(old));
    end if;
    if tg_op in ('UPDATE', 'INSERT') and authz.relationship_applies(r, new) then
        after := authz.relationship_of(r, to_jsonb(new));
        -- a relation the model does not have fails here, where the app sees it, rather than when
        -- the engine refuses it after the commit
        if after is not null and r.relations is not null and not after[2] = any (r.relations) then
            raise exception 'relation % is not one of %', after[2], array_to_string(r.relations, ', ')
                using errcode = 'check_violation',
                      hint = format('%s declares the relations its rows may name (authz.relate relations => ...)', tg_relid::regclass);
        end if;
    end if;
    if before is not distinct from after then
        return null;
    end if;
    if before is not null then
        insert into authz.outbox (op, object, relation, subject) values ('delete', before[1], before[2], before[3]);
    end if;
    if after is not null then
        insert into authz.outbox (op, object, relation, subject) values ('write', after[1], after[2], after[3]);
    end if;
    perform pg_notify('pg_auth_outbox', '');
    return null;
end
$$;

-- Declare that `rel`'s rows are relationships. Each part of a row's relationship is fixed or read
-- from a column: the object (object_type or object_type_column, object_column or object_id), the
-- relation (relation or relation_column), the subject (subject_type or subject_type_column,
-- subject_column or subject_id) and the subject's relation (subject_relation or
-- subject_relation_column, optional). For example:
--
--   -- org:<org_id>#<role>@user:<user_id>, the role a member of a fixed set
--   select authz.relate('app.memberships', object_type => 'org', object_column => 'org_id',
--                       relation_column => 'role', subject_type => 'user', subject_column => 'user_id',
--                       relations => array['admin', 'member']);
--   -- one shares table, a declaration per kind of grantee; everyone is user:*
--   select authz.relate('app.shares', name => 'team', filter => 'grantee_kind = ''team''',
--                       object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
--                       subject_type => 'team', subject_column => 'grantee_id', subject_relation => 'member');
--   select authz.relate('app.shares', name => 'public', filter => 'grantee_kind = ''public''',
--                       object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
--                       subject_type => 'user', subject_id => '*');
--
-- Rows already there are delivered too. Give the table a unique key over these columns: two rows
-- that are one relationship would race each other's deletes.
create function authz.relate(
    rel regclass,
    object_type text default null,
    object_column name default null,
    subject_type text default null,
    subject_column name default null,
    relation text default null,
    relation_column name default null,
    subject_relation text default null,
    name text default 'pg_auth',
    object_type_column name default null,
    object_id text default null,
    subject_type_column name default null,
    subject_id text default null,
    subject_relation_column name default null,
    filter text default null,
    relations text[] default null
) returns bigint
language plpgsql volatile as $$
#variable_conflict use_column
declare
    r authz.relationships;
    col name;
    queued bigint;
    bad text;
begin
    foreach bad in array array[
        case when (relate.object_type is null) = (relate.object_type_column is null)
             then 'the object''s type: object_type => ... or object_type_column => ...' end,
        case when (relate.object_column is null) = (relate.object_id is null)
             then 'the object''s id: object_column => ... or object_id => ...' end,
        case when (relate.relation is null) = (relate.relation_column is null)
             then 'the relation: relation => ... or relation_column => ...' end,
        case when (relate.subject_type is null) = (relate.subject_type_column is null)
             then 'the subject''s type: subject_type => ... or subject_type_column => ...' end,
        case when (relate.subject_column is null) = (relate.subject_id is null)
             then 'the subject''s id: subject_column => ... or subject_id => ...' end,
        case when relate.subject_relation is not null and relate.subject_relation_column is not null
             then 'the subject''s relation: subject_relation => ... or subject_relation_column => ..., not both' end
    ] loop
        if bad is not null then
            raise exception 'a relationship needs exactly one of each part; %', bad;
        end if;
    end loop;
    foreach col in array array[relate.object_type_column, relate.object_column, relate.relation_column, relate.subject_type_column,
                               relate.subject_column, relate.subject_relation_column] loop
        if col is not null and not exists (select 1 from pg_attribute where attrelid = relate.rel and attname = col and attnum > 0 and not attisdropped) then
            raise exception 'column % does not exist on %', col, relate.rel;
        end if;
    end loop;
    if relate.filter is not null then
        -- a filter that does not parse, or names a missing column, fails here rather than on a write
        execute format('select 1 from %s where %s limit 0', relate.rel, relate.filter);
    end if;
    insert into authz.relationships (rel, name, object_type, object_type_column, object_id, object_column, relation, relation_column,
                                     subject_type, subject_type_column, subject_id, subject_column, subject_relation,
                                     subject_relation_column, filter, relations)
    values (relate.rel, relate.name, relate.object_type, relate.object_type_column, relate.object_id, relate.object_column,
            relate.relation, relate.relation_column, relate.subject_type, relate.subject_type_column, relate.subject_id,
            relate.subject_column, relate.subject_relation, relate.subject_relation_column, relate.filter, relate.relations)
    on conflict on constraint relationships_pkey do update set
        object_type = excluded.object_type, object_type_column = excluded.object_type_column, object_id = excluded.object_id,
        object_column = excluded.object_column, relation = excluded.relation, relation_column = excluded.relation_column,
        subject_type = excluded.subject_type, subject_type_column = excluded.subject_type_column, subject_id = excluded.subject_id,
        subject_column = excluded.subject_column, subject_relation = excluded.subject_relation,
        subject_relation_column = excluded.subject_relation_column, filter = excluded.filter, relations = excluded.relations
    returning * into r;
    execute format('create or replace trigger %I after insert or update or delete on %s for each row execute function authz.relationship_changed(%L)',
                   'pg_auth_relate_' || relate.name, relate.rel, relate.name);
    -- the rows already there, which must name allowed relations too
    execute format('select t[2] from (select authz.relationship_of($1, to_jsonb(x)) t from %s x where %s) s
                    where t is not null and ($1).relations is not null and not t[2] = any (($1).relations) limit 1',
                   relate.rel, coalesce(relate.filter, 'true')) into bad using r;
    if bad is not null then
        raise exception 'a row of % names relation %, not one of %', relate.rel, bad, array_to_string(relate.relations, ', ')
            using errcode = 'check_violation';
    end if;
    execute format('insert into authz.outbox (op, object, relation, subject)
                    select ''write'', t[1], t[2], t[3] from (select authz.relationship_of($1, to_jsonb(x)) t from %s x where %s) s where t is not null',
                   relate.rel, coalesce(relate.filter, 'true')) using r;
    get diagnostics queued = row_count;
    perform pg_notify('pg_auth_outbox', '');
    return queued;
end
$$;

-- Stop delivering `rel`'s rows (the relationships already in the engine stay there).
create function authz.unrelate(rel regclass, name text default 'pg_auth') returns void
language plpgsql volatile as $$
#variable_conflict use_column
begin
    execute format('drop trigger if exists %I on %s', 'pg_auth_relate_' || unrelate.name, unrelate.rel);
    delete from authz.relationships where relationships.rel = unrelate.rel and relationships.name = unrelate.name;
end
$$;

-- For the sidecar: the oldest changes not delivered yet, in order, to the one sidecar delivering
-- (an advisory lock held until the caller's transaction ends); nothing while another delivers.
create function authz.outbox_claim(p_limit int) returns table (id bigint, op text, object text, relation text, subject text)
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
begin
    if not pg_try_advisory_xact_lock(hashtext('pg_auth.outbox')) then
        return;
    end if;
    return query select o.id, o.op, o.object::text, o.relation::text, o.subject::text from authz.outbox o
                 where o.error is null order by o.id limit p_limit;
end
$$;

-- For the sidecar: these changes reached the engine. By id, never by range: a transaction that
-- commits late holds a smaller id than changes already delivered.
create function authz.outbox_done(p_ids bigint[]) returns bigint
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    with done as (delete from authz.outbox where id = any(p_ids) returning 1)
    select count(*) from done
$$;

-- For the sidecar: the engine refused this change for good (an invalid relationship): set it
-- aside, so the changes after it are delivered.
create function authz.outbox_failed(p_id bigint, p_error text) returns void
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    update authz.outbox set error = p_error where id = p_id
$$;

revoke all on authz.relationships, authz.outbox from public;
revoke execute on function authz.relate(regclass, text, name, text, name, text, name, text, text, name, text, name, text, name, text, text[]) from public;
revoke execute on function authz.unrelate(regclass, text) from public;
revoke execute on function authz.outbox_claim(int) from public;
revoke execute on function authz.outbox_done(bigint[]) from public;
revoke execute on function authz.outbox_failed(bigint, text) from public;
