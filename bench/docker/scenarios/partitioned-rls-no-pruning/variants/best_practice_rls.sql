-- Best plain RLS can do: InitPlan array (indexable = ANY). Partitions are still not pruned because
-- the array is only known at run time; the catalog fix is a literal tenant in the query (app layer).
-- Policies are also placed on every leaf so direct access is not a bypass.
do $do$
declare t regclass;
begin
  for t in select unnest(array['app.documents_p16'::regclass, 'app.documents_p128'::regclass])
           union select relid from pg_partition_tree('app.documents_p16')
           union select relid from pg_partition_tree('app.documents_p128') loop
    execute format('alter table %s enable row level security', t);
    execute format($p$create policy p on %s for select to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))))$p$, t);
  end loop;
end $do$;
