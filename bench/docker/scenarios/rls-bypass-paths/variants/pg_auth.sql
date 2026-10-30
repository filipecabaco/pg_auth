-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- common start state (variants share one container): late partition dropped, role/matview reset
alter role app_bypass bypassrls;
delete from authz_internal.bindings;
drop table if exists app.docs_p_late;
do $$ begin
  if (select relkind from pg_class where oid = to_regclass('app.doc_counts')) = 'm' then drop materialized view app.doc_counts;
  else drop table if exists app.doc_counts; end if;
end $$;
create materialized view app.doc_counts as select org_id, count(*) as n from app.docs group by org_id;
alter materialized view app.doc_counts owner to app_owner;
-- pg_auth with the naive layout: protect the tables (FORCE is the default, so owners are subject too),
-- keep the matview and the BYPASSRLS role, create a partition AFTER protecting (event trigger).
select authz_internal.protect('app.docs', 'org', 'org_id', 'member');
select authz_internal.protect('app.docs_p', 'org', 'org_id', 'member');
create table app.docs_p_late partition of app.docs_p for values in (6, 7, 8, 9, 10);
insert into app.docs_p select i, 1 + (i % 10), 'doc ' || i from generate_series(1, 2000) i where 1 + (i % 10) > 5;
