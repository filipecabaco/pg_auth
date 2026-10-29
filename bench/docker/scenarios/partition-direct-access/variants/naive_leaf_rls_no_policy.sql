-- Partial fix people try: enable RLS on the leaves but forget the policy: the leaf is default-deny (0 rows).
alter table app.documents_p enable row level security;
create policy p on app.documents_p for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
do $do$
declare t regclass;
begin
  for t in select relid from pg_partition_tree('app.documents_p') where isleaf loop
    execute format('alter table %s enable row level security', t);
  end loop;
end $do$;
