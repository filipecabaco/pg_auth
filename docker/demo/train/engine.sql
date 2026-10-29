-- Hot-path training, before the snapshot (start.sh runs this for each engine's database): the
-- demo's three steps as anne and bob, rolled back, so a visitor's first clicks find warm code,
-- catalogs, caches and connections. Output goes nowhere.
\o /dev/null
select current_database() = 'cerbos' as cerbos \gset
begin;
set local client_min_messages = error;
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
reset role;
rollback;
\o
