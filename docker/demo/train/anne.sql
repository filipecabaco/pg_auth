-- Hot-path training (see engine.sql), in anne's own session: her steps, rolled back
\o /dev/null
begin;
select * from documents;
insert into memberships values (1, 'bob', 'member');
rollback;
set application_name = 'trained';
\o
