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
    object_type      text collate "C" not null,
    object_column    name not null,
    -- the relation: fixed, or read from a column (a membership's role)
    relation         text collate "C",
    relation_column  name,
    check ((relation is null) <> (relation_column is null)),
    subject_type     text collate "C" not null,
    subject_column   name not null,
    -- a set of subjects rather than one (group:eng#member)
    subject_relation text collate "C",
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
-- of its columns is null.
create function authz.relationship_of(r authz.relationships, row_ jsonb) returns text[]
language sql immutable as $$
    select case when row_ ->> r.object_column is null or row_ ->> r.subject_column is null
                     or coalesce(r.relation, row_ ->> r.relation_column) is null then null
                else array[r.object_type || ':' || (row_ ->> r.object_column),
                           coalesce(r.relation, row_ ->> r.relation_column),
                           r.subject_type || ':' || (row_ ->> r.subject_column) || coalesce('#' || r.subject_relation, '')]
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
    if tg_op in ('UPDATE', 'DELETE') then
        before := authz.relationship_of(r, to_jsonb(old));
    end if;
    if tg_op in ('UPDATE', 'INSERT') then
        after := authz.relationship_of(r, to_jsonb(new));
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

-- Declare that `rel`'s rows are relationships: each row says `subject_type:<subject_column>` has
-- `relation` (or the row's `relation_column`) on `object_type:<object_column>`, e.g.
--
--   select authz.relate('app.memberships', object_type => 'org', object_column => 'org_id',
--                       relation_column => 'role', subject_type => 'user', subject_column => 'user_id');
--
-- Rows already there are delivered too. Give the table a unique key over these columns: two rows
-- that are one relationship would race each other's deletes.
create function authz.relate(
    rel regclass,
    object_type text,
    object_column name,
    subject_type text,
    subject_column name,
    relation text default null,
    relation_column name default null,
    subject_relation text default null,
    name text default 'pg_auth'
) returns bigint
language plpgsql volatile as $$
#variable_conflict use_column
declare
    r authz.relationships;
    col name;
    queued bigint;
begin
    if (relate.relation is null) = (relate.relation_column is null) then
        raise exception 'a relationship has a fixed relation (relation => ...) or reads it from a column (relation_column => ...), not both or neither';
    end if;
    foreach col in array array[relate.object_column, relate.subject_column, relate.relation_column] loop
        if col is not null and not exists (select 1 from pg_attribute where attrelid = relate.rel and attname = col and attnum > 0 and not attisdropped) then
            raise exception 'column % does not exist on %', col, relate.rel;
        end if;
    end loop;
    insert into authz.relationships values (relate.rel, relate.name, relate.object_type, relate.object_column, relate.relation,
                                            relate.relation_column, relate.subject_type, relate.subject_column, relate.subject_relation)
    on conflict on constraint relationships_pkey do update set
        object_type = excluded.object_type, object_column = excluded.object_column, relation = excluded.relation,
        relation_column = excluded.relation_column, subject_type = excluded.subject_type,
        subject_column = excluded.subject_column, subject_relation = excluded.subject_relation
    returning * into r;
    execute format('create or replace trigger %I after insert or update or delete on %s for each row execute function authz.relationship_changed(%L)',
                   'pg_auth_relate_' || relate.name, relate.rel, relate.name);
    -- the rows already there
    execute format('insert into authz.outbox (op, object, relation, subject)
                    select ''write'', t[1], t[2], t[3] from (select authz.relationship_of($1, to_jsonb(x)) t from %s x) s where t is not null',
                   relate.rel) using r;
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
revoke execute on function authz.relate(regclass, text, name, text, name, text, name, text, text) from public;
revoke execute on function authz.unrelate(regclass, text) from public;
revoke execute on function authz.outbox_claim(int) from public;
revoke execute on function authz.outbox_done(bigint[]) from public;
revoke execute on function authz.outbox_failed(bigint, text) from public;
