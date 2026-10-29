-- @role: superuser
drop table if exists app.docs_p_late;
create table app.docs_p_late partition of app.docs_p for values in (6, 7, 8, 9, 10);
insert into app.docs_p select i, 1 + (i % 10), 'doc ' || i from generate_series(1, 2000) i where 1 + (i % 10) > 5;
