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

-- The page calls this when the demo boots and when a visitor switches engines: every table back as
-- the demo starts (rows added removed, rows deleted put back), so each visit tells the same story.
-- It runs as its owner (a superuser), and every change it makes reaches the engine like any other.
create function demo_reset() returns void language sql security definer set search_path = public as $$
  delete from shares;
  delete from memberships where (org_id, user_id) not in ((1, 'anne'), (2, 'carl'));
  insert into memberships values (1, 'anne', 'admin'), (2, 'carl', 'admin')
    on conflict (org_id, user_id) do update set role = excluded.role where memberships.role <> excluded.role;
  delete from documents where id not in (1, 2, 3, 4, 5);
  insert into documents values (1, 1, 'Roadmap'), (2, 1, 'Budget'), (3, 1, 'Launch plan'), (4, 2, 'Hiring'), (5, 2, 'Pricing')
    on conflict (id) do update set org_id = excluded.org_id, title = excluded.title
    where (documents.org_id, documents.title) is distinct from (excluded.org_id, excluded.title);
$$;
grant execute on function demo_reset() to authenticated;
