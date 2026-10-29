-- The OpenFGA and SpiceDB databases: a team document editor whose sharing lives in tables.
-- Run with -v engine=... -v rules_name=... -v rules_file=...
create extension pg_auth;
select authz.configure(subject => 'jwt');

create table documents (id int primary key, org_id int not null, title text not null);
create index on documents (org_id);
create table memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id));
create table shares (doc_id int not null, user_id text not null, expires_at timestamptz, primary key (doc_id, user_id));
grant select, insert, update, delete on documents, memberships, shares to authenticated;

insert into documents values (1, 1, 'Roadmap'), (2, 1, 'Budget'), (3, 1, 'Launch plan'), (4, 2, 'Hiring'), (5, 2, 'Pricing');
insert into memberships values (1, 'anne', 'admin'), (2, 'carl', 'admin');

-- the engine's rules, in its own language, with notes saying where each type's rows live: the
-- notes protect these tables and make their rows relationships (the Dockerfile passes engine and file)
\set rules `cat :rules_file`
select authz.set_rules(:'engine', :'rules_name', :'rules');

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
