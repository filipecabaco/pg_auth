-- @role: superuser
-- No RLS: the membership check is written into each statement; this is the expected result of
-- every statement (rows affected / returned).
select 1;
