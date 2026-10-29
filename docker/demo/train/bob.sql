-- Hot-path training (see admin.sql): bob's statements, rolled back
\o /dev/null
begin;
select * from documents;
select * from memberships;
rollback;
set application_name = 'trained';
\o
