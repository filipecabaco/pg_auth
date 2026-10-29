-- Hot-path training (see admin.sql): anne's statements, rolled back
\o /dev/null
begin;
select * from documents;
select * from memberships;
insert into memberships values (1, 'bob', 'member');
rollback;
set application_name = 'trained';
\o
