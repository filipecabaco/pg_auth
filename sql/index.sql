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

-- These functions resolve the current subject themselves, so a caller cannot read, or probe, another
-- subject's set.

-- The current subject's fresh set for (p_type, p_permission): its id, size and when this fill
-- finished (a refill keeps the id: the pair names the contents), or no row.
create function authz.indexed_set(p_type text, p_permission text, out set_id bigint, out size bigint, out filled_at timestamptz)
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select s.set_id, s.size, s.filled_at
    from authz_internal.permission_sets s
    where s.subject = authz.subject_type() || ':' || authz.subject_id()
      and s.object_type = p_type and s.permission = p_permission and s.status = 'fresh'
$$;

-- Which of p_ids are in set p_set, or NULL when p_set is not the current subject's fresh set (it
-- may have gone stale or be refilling since it was found: the caller then asks the engine).
-- One primary key probe per id: cheap for a few ids, where loading the whole set is not.
create function authz.indexed_lookup(p_set bigint, p_ids text[]) returns text[]
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select case when exists (select 1 from authz_internal.permission_sets s
                             where s.set_id = p_set and s.status = 'fresh'
                               and s.subject = authz.subject_type() || ':' || authz.subject_id())
           then (select coalesce(array_agg(p.object_id), '{}') from authz_internal.permitted p
                 where p.set_id = p_set and p.object_id = any(p_ids))
           end
$$;

-- The current subject's whole fresh set for (p_type, p_permission), or NULL when there is none.
create function authz.indexed_ids(p_type text, p_permission text) returns text[]
language sql stable security definer set search_path = pg_catalog, pg_temp as $$
    select (select coalesce(array_agg(p.object_id), '{}') from authz_internal.permitted p where p.set_id = s.set_id)
    from authz_internal.permission_sets s
    where s.subject = authz.subject_type() || ':' || authz.subject_id()
      and s.object_type = p_type and s.permission = p_permission and s.status = 'fresh'
$$;

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

