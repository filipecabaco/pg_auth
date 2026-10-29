-- The Cerbos database: the same documents, decided on their own fields (owner, status).
create extension pg_auth;
select authz.configure(subject => 'jwt');

create table documents (id int primary key, title text not null, owner text not null, status text not null default 'draft');
create index on documents (owner);
grant select, insert, update, delete on documents to authenticated;

insert into documents values (1, 'Roadmap', 'anne', 'published'), (2, 'Budget', 'anne', 'draft'),
  (3, 'Launch plan', 'anne', 'draft'), (4, 'Hiring', 'carl', 'published'), (5, 'Pricing', 'carl', 'draft');

-- read: anyone signed in reads published documents and their own drafts; update: the owner
select authz.protect('documents', type => 'document', id => 'id', read => 'read', write => 'update',
                     attributes => array['owner', 'status']);

-- The page calls this when a visitor switches to this engine: the data back as the demo starts,
-- so its steps tell the same story on a second visit. It runs as its owner (a superuser).
create function demo_reset() returns void language sql security definer set search_path = public as $$
  update documents d set title = t.title, owner = t.owner, status = t.status
  from (values (1, 'Roadmap', 'anne', 'published'), (2, 'Budget', 'anne', 'draft'), (3, 'Launch plan', 'anne', 'draft'),
               (4, 'Hiring', 'carl', 'published'), (5, 'Pricing', 'carl', 'draft')) t(id, title, owner, status)
  where d.id = t.id;
$$;
grant execute on function demo_reset() to authenticated;
