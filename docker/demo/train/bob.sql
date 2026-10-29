-- Hot-path training (see engine.sql), in bob's own session: his step, rolled back
\o /dev/null
begin;
select * from documents;
rollback;
set application_name = 'trained';
\o
