-- @role: superuser
-- No RLS. The functions still SET ROLE bench_a/bench_b but there are no policies, so this is the
-- floor (plans do not depend on the role). Results differ from the RLS variants because nothing
-- filters rows: the loop returns every PK hit (expected mismatch, not a bug).
select 1;
