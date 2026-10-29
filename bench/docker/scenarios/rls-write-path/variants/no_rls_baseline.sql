-- @role: superuser
-- No RLS: queries run as superuser with the permission filter written into the SQL
-- (see no_rls_baseline.queries.sql): read = tenant member, write = tenant editor.
select 1;
