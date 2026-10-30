-- The Cerbos database: the same documents, decided on their own fields (owner, status).
create extension pg_auth;
select authz.configure(subject => 'jwt');

create table documents (id int primary key, title text not null, owner text not null, status text not null default 'draft');
create index on documents (owner);
grant select, insert, update, delete on documents to authenticated;

insert into documents values (1, 'Roadmap', 'anne', 'published'), (2, 'Budget', 'anne', 'draft'),
  (3, 'Launch plan', 'anne', 'draft'), (4, 'Hiring', 'carl', 'published'), (5, 'Pricing', 'carl', 'draft');

-- the policy, and the mapping protecting this table: anyone signed in reads published documents and
-- their own drafts, and only the owner edits
\set rules `cat /etc/demo/rules/cerbos.yaml`
\set mapping `cat /etc/demo/mappings/cerbos.yaml`
select authz.set_rules('cerbos', 'resource/document', mapping => :'mapping', body => :'rules');

-- The page calls this when the demo boots and when a visitor switches engines: every table back as
-- the demo starts (rows added removed, rows deleted put back), so each visit tells the same story.
-- It runs as its owner (a superuser).
create function demo_reset() returns void language sql security definer set search_path = pg_catalog, public, pg_temp as $$
  delete from documents where id not in (1, 2, 3, 4, 5);
  insert into documents values (1, 'Roadmap', 'anne', 'published'), (2, 'Budget', 'anne', 'draft'),
    (3, 'Launch plan', 'anne', 'draft'), (4, 'Hiring', 'carl', 'published'), (5, 'Pricing', 'carl', 'draft')
    on conflict (id) do update set title = excluded.title, owner = excluded.owner, status = excluded.status
    where (documents.title, documents.owner, documents.status) is distinct from (excluded.title, excluded.owner, excluded.status);
$$;
grant execute on function demo_reset() to authenticated;
