-- Permission index (docs/proposals/permission-index.md, phase A): the full permission sets of
-- subjects with more objects than the engine lists, kept in Postgres so their rows are answered
-- without asking the engine. Derived data: the engine stays the source of truth.
--
-- The sidecar fills a set by streaming the engine's list (authz_sidecar.index_begin / index_add /
-- index_finish) and marks sets stale from the engine's change feed (authz_sidecar.index_invalidate).
-- pg_auth answers from a fresh set only; while a set is missing, filling or stale it asks the
-- engine, so a revocation is never missed beyond the feed's delay.

create table authz_internal.permission_sets (
    set_id      bigserial primary key,
    subject     text collate "C" not null,
    object_type text collate "C" not null,
    permission  text collate "C" not null,
    status      text not null default 'filling' check (status in ('filling', 'fresh', 'stale')),
    -- the engine position the set was read at, as the sidecar reports it (informational)
    revision    text,
    filled_at   timestamptz,
    -- objects in the set, counted when it becomes fresh
    size        bigint,
    unique (subject, object_type, permission)
);

create table authz_internal.permitted (
    set_id    bigint not null references authz_internal.permission_sets on delete cascade,
    object_id text collate "C" not null,
    primary key (set_id, object_id)
);

-- pg_auth reads both tables itself, through their indexes (src/pg/index.rs), for the current
-- subject only: nobody else reads them, and src/pg/index.rs relies on the two indexes' names.

-- Writer API for the sidecar. Grant execute to the sidecar's role; nobody else needs it.

-- Start (re)filling a set: it is not used until index_finish. Returns its id.
create function authz_sidecar.index_begin(p_subject text, p_type text, p_permission text, p_revision text)
returns bigint
language plpgsql volatile security definer set search_path = pg_catalog, pg_temp as $$
declare
    id bigint;
begin
    insert into authz_internal.permission_sets (subject, object_type, permission, status, revision)
    values (p_subject, p_type, p_permission, 'filling', p_revision)
    on conflict (subject, object_type, permission)
        do update set status = 'filling', revision = excluded.revision, filled_at = null
    returning set_id into id;
    delete from authz_internal.permitted where set_id = id;
    return id;
end
$$;

create function authz_sidecar.index_add(p_set bigint, p_ids text[]) returns void
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    insert into authz_internal.permitted (set_id, object_id)
    select p_set, i from unnest(p_ids) i
    on conflict do nothing
$$;

-- Make a filled set usable, unless it was invalidated meanwhile (status no longer 'filling').
create function authz_sidecar.index_finish(p_set bigint) returns boolean
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    update authz_internal.permission_sets
    set status = 'fresh', filled_at = now(), size = (select count(*) from authz_internal.permitted where set_id = p_set)
    where set_id = p_set and status = 'filling'
    returning true
$$;

-- A fill that failed: the set is stale (asked for again), not left filling.
create function authz_sidecar.index_abort(p_set bigint) returns void
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    update authz_internal.permission_sets set status = 'stale' where set_id = p_set and status = 'filling'
$$;

-- How far the engine's change feed has been read: every change up to `position` has marked the
-- sets stale. A sidecar's feed starts from it, so a restart, or another sidecar starting, keeps
-- the sets when nothing changed meanwhile; without one it must treat every set as stale. Any
-- sidecar may move it back (one that lags): an earlier position is still one everything before
-- was handled by, and is only replayed again.
create table authz_internal.feed_positions (
    engine   text primary key,
    -- the engine's own: OpenFGA's continuation token (or the time to read changes from), SpiceDB's
    -- watch cursor
    position text not null,
    at       timestamptz not null default now()
);

create function authz_sidecar.feed_position(p_engine text) returns text
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select f.position from authz_internal.feed_positions f where f.engine = p_engine
$$;

create function authz_sidecar.feed_reached(p_engine text, p_position text) returns void
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    insert into authz_internal.feed_positions (engine, position) values (p_engine, p_position)
    on conflict (engine) do update set position = excluded.position, at = now()
$$;

-- The sets fresh now (filled before a sidecar restart): the ones the change feed guards.
create function authz_sidecar.index_fresh() returns table (subject text, object_type text, permission text, size bigint)
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select s.subject, s.object_type, s.permission, s.size from authz_internal.permission_sets s where s.status = 'fresh'
$$;

-- The engine changed: no set is used until refilled. Coarse on purpose (any change may reach any
-- set through inheritance); returns how many sets were fresh or filling.
create function authz_sidecar.index_invalidate() returns bigint
language sql volatile security definer set search_path = pg_catalog, pg_temp as $$
    with changed as (
        update authz_internal.permission_sets set status = 'stale' where status <> 'stale' returning 1
    )
    select count(*) from changed
$$;

