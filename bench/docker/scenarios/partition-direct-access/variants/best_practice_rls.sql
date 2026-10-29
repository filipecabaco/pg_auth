-- Fix: the same policy on the parent AND on every leaf (a creation hook / event trigger must keep
-- doing this for future partitions).
do $do$
declare t regclass;
begin
  for t in select 'app.documents_p'::regclass union select relid from pg_partition_tree('app.documents_p') where isleaf loop
    execute format('alter table %s enable row level security', t);
    execute format($p$create policy p on %s for select to bench_user
      using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))))$p$, t);
  end loop;
end $do$;
