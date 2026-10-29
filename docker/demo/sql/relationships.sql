-- The OpenFGA and SpiceDB databases: a team document editor whose sharing lives in tables.
create extension pg_auth;
select authz.configure(subject => 'jwt');

create table documents (id int primary key, org_id int not null, title text not null);
create index on documents (org_id);
create table memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id));
create table shares (doc_id int not null, user_id text not null, primary key (doc_id, user_id));
grant select, insert, update, delete on documents, memberships, shares to authenticated;

insert into documents values (1, 1, 'Roadmap'), (2, 1, 'Budget'), (3, 1, 'Launch plan'), (4, 2, 'Hiring'), (5, 2, 'Pricing');
insert into memberships values (1, 'anne', 'admin'), (2, 'carl', 'admin');

-- each row is a relationship in the engine: a document's org, a member's role, a share
select authz.relate('documents', object_type => 'document', object_column => 'id', relation => 'org',
                    subject_type => 'org', subject_column => 'org_id');
select authz.relate('memberships', object_type => 'org', object_column => 'org_id', relation_column => 'role',
                    subject_type => 'user', subject_column => 'user_id');
select authz.relate('shares', object_type => 'document', object_column => 'doc_id', relation => 'shared_with',
                    subject_type => 'user', subject_column => 'user_id');

-- who may read and write each table: a document needs viewer to read and editor to write, only an
-- org's admins add members, anyone who edits a document shares it
select authz.protect('documents', type => 'document', id => 'id', read => 'viewer', write => 'editor');
select authz.protect('memberships', type => 'org', id => 'org_id', read => 'can_read', write => 'can_manage');
select authz.protect('shares', type => 'document', id => 'doc_id', read => 'viewer', write => 'editor');

-- The page calls this when a visitor switches to this engine: the data back as the demo starts,
-- so its steps tell the same story on a second visit. It runs as its owner (a superuser), and the
-- membership it removes is delivered to the engine like any other.
create function demo_reset() returns void language sql security definer set search_path = public as $$
  delete from memberships where (org_id, user_id) not in ((1, 'anne'), (2, 'carl'));
  delete from shares;
  update documents d set title = t.title
  from (values (1, 'Roadmap'), (2, 'Budget'), (3, 'Launch plan'), (4, 'Hiring'), (5, 'Pricing')) t(id, title)
  where d.id = t.id and d.title <> t.title;
$$;
grant execute on function demo_reset() to authenticated;
