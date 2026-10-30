-- authz.lint(): row level security problems in this database, most severe first.
--   P0xx  found by analysing each policy expression (authz_internal.lint_policies, src/lint.rs)
--   R0xx  found from the catalog
create function authz.lint()
returns table (severity text, code text, object text, message text, hint text)
language sql stable as $$
    with user_tables as (
        select c.*
        from pg_class c
        join pg_namespace n on n.oid = c.relnamespace
        where n.nspname not in ('pg_catalog', 'information_schema') and n.nspname not like 'pg\_%'
    ),
    findings as (
        select * from authz_internal.lint_policies()

        union all
        select 'info', 'R001', c.oid::regclass::text,
               'row level security is enabled but there are no policies: non-owners see no rows',
               'add policies, or protect the table with authz_internal.protect()'
        from user_tables c
        where c.relrowsecurity and c.relkind in ('r', 'p')
          and not exists (select 1 from pg_policy p where p.polrelid = c.oid)

        union all
        select 'high', 'R002', ch.oid::regclass::text,
               format('partition of %s without its own policies: querying it directly skips the parent''s policies',
                      par.oid::regclass),
               'enable row level security and copy the policies to every partition (authz_internal.protect does this)'
        from pg_inherits i
        join user_tables par on par.oid = i.inhparent
        join pg_class ch on ch.oid = i.inhrelid
        where par.relrowsecurity
          and (not ch.relrowsecurity or not exists (select 1 from pg_policy p where p.polrelid = ch.oid))

        union all
        select distinct 'high', 'R003', v.oid::regclass::text,
               format('view reads %s, which has row level security, but runs with its owner''s privileges, '
                      'so the policies do not apply to callers', t.oid::regclass),
               'alter view ... set (security_invoker = true)'
        from user_tables v
        join pg_rewrite r on r.ev_class = v.oid
        join pg_depend d on d.classid = 'pg_rewrite'::regclass and d.objid = r.oid
                        and d.refclassid = 'pg_class'::regclass
        join pg_class t on t.oid = d.refobjid and t.oid <> v.oid
        join pg_roles o on o.oid = v.relowner
        where v.relkind = 'v' and t.relrowsecurity
          and not exists (select 1 from unnest(v.reloptions) opt
                          where opt ~* '^security_invoker=(true|on|yes|1)$')
          and (o.rolsuper or o.rolbypassrls or (v.relowner = t.relowner and not t.relforcerowsecurity))

        union all
        select 'medium', 'R004', c.oid::regclass::text,
               'UPDATE policies but no SELECT policy: UPDATE ... WHERE cannot see rows, so updates affect 0 rows',
               'add a SELECT policy covering the rows that may be updated'
        from user_tables c
        where exists (select 1 from pg_policy p where p.polrelid = c.oid and p.polcmd = 'w')
          and not exists (select 1 from pg_policy p where p.polrelid = c.oid and p.polcmd in ('r', '*'))
    )
    select f.severity, f.code, f.object, f.message, f.hint
    from findings f
    order by case f.severity when 'high' then 0 when 'medium' then 1 else 2 end, f.object, f.code
$$;

revoke execute on function authz.lint() from public;
