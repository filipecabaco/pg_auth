-- The Cerbos database: the same documents, decided on their own fields (owner, status).
create extension pg_auth;
select authz.configure(subject => 'jwt');

create table documents (id int primary key, title text not null, owner text not null, status text not null default 'draft');
create index on documents (owner);
grant select, insert, update, delete on documents to authenticated;

insert into documents values (1, 'Roadmap', 'anne', 'published'), (2, 'Budget', 'anne', 'draft'),
  (3, 'Launch plan', 'anne', 'draft'), (4, 'Hiring', 'carl', 'published'), (5, 'Pricing', 'carl', 'draft');
