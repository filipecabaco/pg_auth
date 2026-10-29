-- common start state (variants share one container): late partition recreated, role/matview reset
alter role app_bypass bypassrls;
drop table if exists app.docs_p_late;
do $$ begin
  if (select relkind from pg_class where oid = to_regclass('app.doc_counts')) = 'm' then drop materialized view app.doc_counts;
  else drop table if exists app.doc_counts; end if;
end $$;
create materialized view app.doc_counts as select org_id, count(*) as n from app.docs group by org_id;
alter materialized view app.doc_counts owner to app_owner;
-- Catalog fixes: policy for every role + FORCE (owner included), policy on every leaf (also the
-- late one), matview replaced by an RLS-protected table, app traffic never uses a BYPASSRLS role.
alter role app_bypass nobypassrls;
alter table app.docs enable row level security;  alter table app.docs force row level security;
create policy p on app.docs for select
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
alter table app.docs_p enable row level security; alter table app.docs_p force row level security;
create policy p on app.docs_p for select
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
alter table app.docs_p1 enable row level security; alter table app.docs_p1 force row level security;
create policy p on app.docs_p1 for select
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
-- a partition added AFTER the policies were installed (e.g. by a later migration)
create table app.docs_p_late partition of app.docs_p for values in (6, 7, 8, 9, 10);
insert into app.docs_p select i, 1 + (i % 10), 'doc ' || i from generate_series(1, 2000) i where 1 + (i % 10) > 5;
alter table app.docs_p_late enable row level security; alter table app.docs_p_late force row level security;
create policy p on app.docs_p_late for select
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
drop materialized view app.doc_counts;
create table app.doc_counts as select org_id, count(*) as n from app.docs group by org_id;
alter table app.doc_counts owner to app_owner;
alter table app.doc_counts enable row level security; alter table app.doc_counts force row level security;
create policy p on app.doc_counts for select
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
-- FK probe: referenced rows are checked with owner rights; FORCE does not change that.
