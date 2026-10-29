-- @role: superuser
-- No RLS; the permission filter is written into every query (also the leaf queries) so the
-- baseline shows what the *correct* answer is.
select 1;
