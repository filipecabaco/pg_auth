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

-- only an org's admins add members; anyone who edits a document shares it. `documents` itself is
-- left for the demo's first step.
select authz.protect('memberships', type => 'org', id => 'org_id', read => 'can_read', write => 'can_manage');
select authz.protect('shares', type => 'document', id => 'doc_id', read => 'viewer', write => 'editor');
