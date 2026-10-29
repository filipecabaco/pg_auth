-- @role: superuser
-- No RLS: queries run as superuser with the permission filter written into the SQL
-- (see no_rls_baseline.queries.sql). The role test reads the same claims the policies read.
select 1;
