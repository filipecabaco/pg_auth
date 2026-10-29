-- Hot-path training, before the snapshot: the demo's own statements in this session, rolled back,
-- so a visitor's first clicks find warm code, catalogs, caches and connections. Output goes nowhere.
-- In a pane it first waits for anne's and bob's panes (protecting documents here would block
-- them); start.sh runs it alone (-v solo=1) for each engine.
\o /dev/null
\if :{?solo}
\else
do $$ begin
  for i in 1..6000 loop
    perform pg_stat_clear_snapshot();  -- pg_stat_activity is read once per transaction otherwise
    exit when (select count(*) from pg_stat_activity where application_name = 'trained') >= 2;
    perform pg_sleep(0.05);
  end loop;
end $$;
\endif
select current_database() = 'cerbos' as cerbos \gset
begin;
set local client_min_messages = error;
\if :cerbos
select authz.protect('documents', type => 'document', id => 'id', read => 'read', write => 'update', attributes => array['owner', 'status']);
\else
select authz.protect('documents', type => 'document', id => 'id', read => 'viewer', write => 'editor');
\endif
set local role anne;
set local request.jwt.claims = '{"sub": "anne", "role": "authenticated"}';
select * from documents;
\if :cerbos
update documents set status = 'published' where id = 2;
\else
insert into memberships values (1, 'bob', 'member');
\endif
reset role;
set local role bob;
set local request.jwt.claims = '{"sub": "bob", "role": "authenticated"}';
select * from documents;
\if :cerbos
update documents set title = 'Mine now' where id = 1;
\endif
reset role;
\if :cerbos
select * from authz.why('documents', '5', as_subject => 'user:bob', subject_roles => array['authenticated']);
\else
select * from authz.why('documents', '4', as_subject => 'user:bob');
\endif
select * from authz.doctor();
rollback;
set application_name = 'trained';
\o
