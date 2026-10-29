-- @role: superuser
-- No RLS: queries run as superuser with the permission filter written into the SQL
-- (see no_rls_baseline.queries.sql). This is the target the other variants try to reach.
select 1;
