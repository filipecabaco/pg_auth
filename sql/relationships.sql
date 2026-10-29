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
    -- a timestamp column: the relationship ends then (null: it does not), deleted from the engine
    -- when it is due, and given to SpiceDB as the relationship's expiration
    expires_column          name,
    -- an OpenFGA condition or SpiceDB caveat the relationship is written with, and the values the
    -- row gives it: {"parameter": "column"}
    condition               text collate "C",
    condition_context       jsonb,
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
    error    text,
    -- a write: when the relationship ends, if it does (SpiceDB enforces it itself)
    expires_at timestamptz,
    -- a delete not delivered before then: a relationship's expiry, scheduled with its write and
    -- dropped when the relationship changes again
    not_before timestamptz,
    -- a write's condition (caveat) and the row's values for it
    condition text,
    condition_context jsonb
);

-- what the sidecar claims: changes to deliver now, by id, and expiries by when they are due
create index outbox_now on authz.outbox (id) where error is null and not_before is null;
create index outbox_expiries on authz.outbox (not_before) where error is null and not_before is not null;
-- a relationship's scheduled expiry, found when the relationship changes again
create index outbox_expiry_of on authz.outbox (object, relation, subject) where not_before is not null;

-- The values the row gives the relationship's condition (`condition_context`), or null; a null
-- column gives none.
create function authz.relationship_context(r authz.relationships, row_ jsonb) returns jsonb
language sql immutable as $$
    select case when r.condition is not null
                then coalesce((select jsonb_object_agg(e.key, row_ -> e.value) from jsonb_each_text(r.condition_context) e
                               where jsonb_typeof(row_ -> e.value) is distinct from 'null'), '{}') end
$$;

-- When the row's relationship ends (`expires_column`), or null.
create function authz.relationship_expires(r authz.relationships, row_ jsonb) returns timestamptz
language sql stable as $$
    select case when r.expires_column is not null then (row_ ->> r.expires_column)::timestamptz end
$$;

-- Record the change from relationship `before` to `after` (either null) in the outbox, `after`
-- ending at `expires` (null: it does not) and written with `condition` and its `context`. An
-- expiry is a delete the outbox holds until it is due; a change to the relationship drops the one
-- scheduled before. `rewrite`: the same relationship with other values for its condition, deleted
-- and written again (OpenFGA refuses to write a tuple again with another condition context).
create function authz.outbox_record(before text[], after text[], expires timestamptz,
                                    condition text default null, context jsonb default null, rewrite boolean default false) returns void
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
begin
    delete from authz.outbox o
    where o.not_before is not null and o.error is null
      and ((o.object = before[1] and o.relation = before[2] and o.subject = before[3])
           or (o.object = after[1] and o.relation = after[2] and o.subject = after[3]));
    if before is not null and (before is distinct from after or rewrite) then
        insert into authz.outbox (op, object, relation, subject) values ('delete', before[1], before[2], before[3]);
    end if;
    if after is not null then
        insert into authz.outbox (op, object, relation, subject, expires_at, condition, condition_context)
        values ('write', after[1], after[2], after[3], expires, outbox_record.condition, outbox_record.context);
        if expires is not null then
            insert into authz.outbox (op, object, relation, subject, not_before) values ('delete', after[1], after[2], after[3], expires);
        end if;
    end if;
end
$$;

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
    expired_before timestamptz;
    expires timestamptz;
    context_before jsonb;
    context_after jsonb;
begin
    select * into r from authz.relationships where rel = tg_relid and name = tg_argv[0];
    if r is null then
        return null;
    end if;
    if tg_op in ('UPDATE', 'DELETE') and authz.relationship_applies(r, old) then
        before := authz.relationship_of(r, to_jsonb(old));
        expired_before := authz.relationship_expires(r, to_jsonb(old));
        context_before := authz.relationship_context(r, to_jsonb(old));
    end if;
    if tg_op in ('UPDATE', 'INSERT') and authz.relationship_applies(r, new) then
        after := authz.relationship_of(r, to_jsonb(new));
        expires := authz.relationship_expires(r, to_jsonb(new));
        context_after := authz.relationship_context(r, to_jsonb(new));
        if expires <= now() then
            after := null;  -- ended already: no relationship
        end if;
        -- a relation the model does not have fails here, where the app sees it, rather than when
        -- the engine refuses it after the commit
        if after is not null and r.relations is not null and not after[2] = any (r.relations) then
            raise exception 'relation % is not one of %', after[2], array_to_string(r.relations, ', ')
                using errcode = 'check_violation',
                      hint = format('the relations %s may name are those its subject can hold in the rules (or authz.relate''s relations)', tg_relid::regclass);
        end if;
    end if;
    if before is not distinct from after and expired_before is not distinct from expires and context_before is not distinct from context_after then
        return null;
    end if;
    perform authz.outbox_record(before, after, expires, r.condition, context_after,
                                before is not distinct from after and context_before is distinct from context_after);
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
--   -- a share in some regions: the in_region condition (caveat) gets them from the row's regions
--   select authz.relate('app.shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
--                       subject_type => 'user', subject_column => 'user_id',
--                       condition => 'in_region', condition_context => '{"allowed": "regions"}');
--   -- a share until its expires_at: deleted from the engine then (SpiceDB also expires it itself)
--   select authz.relate('app.shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
--                       subject_type => 'user', subject_column => 'user_id', expires_column => 'expires_at');
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
    relations text[] default null,
    expires_column name default null,
    condition text default null,
    condition_context jsonb default null
) returns bigint
language plpgsql volatile as $$
#variable_conflict use_column
declare
    r authz.relationships;
    col name;
    queued bigint;
    bad text;
    row_ record;
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
                               relate.subject_column, relate.subject_relation_column, relate.expires_column] loop
        if col is not null and not exists (select 1 from pg_attribute where attrelid = relate.rel and attname = col and attnum > 0 and not attisdropped) then
            raise exception 'column % does not exist on %', col, relate.rel;
        end if;
    end loop;
    if relate.condition_context is not null and relate.condition is null then
        raise exception 'condition_context gives a condition its values: name it (condition => ...)';
    end if;
    for col in select value from jsonb_each_text(coalesce(relate.condition_context, '{}')) loop
        if not exists (select 1 from pg_attribute where attrelid = relate.rel and attname = col and attnum > 0 and not attisdropped) then
            raise exception 'column % does not exist on % (condition_context)', col, relate.rel;
        end if;
    end loop;
    if relate.expires_column is not null and (select atttypid from pg_attribute where attrelid = relate.rel and attname = relate.expires_column)
                                              not in ('timestamptz'::regtype, 'timestamp'::regtype, 'date'::regtype) then
        raise exception 'expires_column % of % is not a timestamp', relate.expires_column, relate.rel
            using hint = 'a timestamptz column: when the relationship ends, null when it does not';
    end if;
    if relate.filter is not null then
        -- a filter that does not parse, or names a missing column, fails here rather than on a write
        execute format('select 1 from %s where %s limit 0', relate.rel, relate.filter);
    end if;
    insert into authz.relationships (rel, name, object_type, object_type_column, object_id, object_column, relation, relation_column,
                                     subject_type, subject_type_column, subject_id, subject_column, subject_relation,
                                     subject_relation_column, filter, relations, expires_column, condition, condition_context)
    values (relate.rel, relate.name, relate.object_type, relate.object_type_column, relate.object_id, relate.object_column,
            relate.relation, relate.relation_column, relate.subject_type, relate.subject_type_column, relate.subject_id,
            relate.subject_column, relate.subject_relation, relate.subject_relation_column, relate.filter, relate.relations,
            relate.expires_column, relate.condition, relate.condition_context)
    on conflict on constraint relationships_pkey do update set
        object_type = excluded.object_type, object_type_column = excluded.object_type_column, object_id = excluded.object_id,
        object_column = excluded.object_column, relation = excluded.relation, relation_column = excluded.relation_column,
        subject_type = excluded.subject_type, subject_type_column = excluded.subject_type_column, subject_id = excluded.subject_id,
        subject_column = excluded.subject_column, subject_relation = excluded.subject_relation,
        subject_relation_column = excluded.subject_relation_column, filter = excluded.filter, relations = excluded.relations,
        expires_column = excluded.expires_column, condition = excluded.condition, condition_context = excluded.condition_context
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
    -- the rows already there that have not ended, each with its expiry
    queued := 0;
    for row_ in execute format('select t, e, c from (select authz.relationship_of($1, to_jsonb(x)) t, authz.relationship_expires($1, to_jsonb(x)) e,
                                       authz.relationship_context($1, to_jsonb(x)) c
                                from %s x where %s) s where t is not null and (e is null or e > now())',
                               relate.rel, coalesce(relate.filter, 'true')) using r loop
        perform authz.outbox_record(null, row_.t, row_.e, r.condition, row_.c);
        queued := queued + 1;
    end loop;
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
create function authz.outbox_claim(p_limit int, p_engine text default null)
returns table (id bigint, op text, object text, relation text, subject text, expires_at text, condition text, condition_context text)
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
begin
    if not pg_try_advisory_xact_lock(hashtext('pg_auth.outbox')) then
        return;
    end if;
    -- relationships wait for the engine's rules stored with them: a relation the engine does not
    -- have yet would be refused
    if p_engine is not null and exists (select 1 from authz.rules(p_engine) r where r.pushed_at is null and r.error is null) then
        return;
    end if;
    -- what is due: changes, and expiries whose time came (expires_at in RFC 3339, as engines take it)
    return query
    select o.id, o.op, o.object::text, o.relation::text, o.subject::text,
           to_char(o.expires_at at time zone 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'), o.condition, o.condition_context::text
    from (select * from (select * from authz.outbox x where x.error is null and x.not_before is null order by x.id limit p_limit) now_
          union all
          select * from (select * from authz.outbox x where x.error is null and x.not_before <= now() order by x.not_before limit p_limit) due) o
    order by o.id limit p_limit;
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
revoke execute on function authz.relate(regclass, text, name, text, name, text, name, text, text, name, text, name, text, name, text, text[], name, text, jsonb) from public;
revoke execute on function authz.outbox_record(text[], text[], timestamptz, text, jsonb, boolean) from public;
revoke execute on function authz.unrelate(regclass, text) from public;
revoke execute on function authz.outbox_claim(int, text) from public;
revoke execute on function authz.outbox_done(bigint[]) from public;
revoke execute on function authz.outbox_failed(bigint, text) from public;
