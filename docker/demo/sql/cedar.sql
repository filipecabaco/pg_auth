-- The Cedar database: the same team document editor, its sharing in tables, decided by Cedar in the
-- sidecar itself. Cedar's policies read the organisation from the row (`principal in
-- resource.org`), so a share carries its document's org too: filled in from the document, so the
-- demo's statements are the same as on OpenFGA and SpiceDB.
create extension pg_auth;
-- Cedar's types are capitalised: the subject is a User
select authz.configure(subject => 'jwt', subject_type => 'User');

create table documents (id int primary key, org_id int not null, title text not null);
create index on documents (org_id);
create table memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id));
create table shares (doc_id int not null, user_id text not null, expires_at timestamptz, org_id int, primary key (doc_id, user_id));
grant select, insert, update, delete on documents, memberships, shares to authenticated;

-- a share's org is its document's (read as the owner: the policy then decides who may share)
create function share_org() returns trigger language plpgsql security definer set search_path = public as $$
begin
  new.org_id := (select d.org_id from documents d where d.id = new.doc_id);
  return new;
end
$$;
create trigger share_org before insert or update of doc_id on shares for each row execute function share_org();

insert into documents values (1, 1, 'Roadmap'), (2, 1, 'Budget'), (3, 1, 'Launch plan'), (4, 2, 'Hiring'), (5, 2, 'Pricing');
insert into memberships values (1, 'anne', 'admin'), (2, 'carl', 'admin');

-- the schema and the policies, in Cedar's own syntax, and with the schema the mapping that
-- protects these tables and makes their rows parents (a member in an org, a user in a document)
\set schema `cat /etc/demo/rules/cedar.cedarschema`
\set policies `cat /etc/demo/rules/cedar.cedar`
\set mapping `cat /etc/demo/mappings/cedar.yaml`
begin;
select authz.set_rules('cedar', 'schema', mapping => :'mapping', body => :'schema');
select authz.set_rules('cedar', 'documents', :'policies');
commit;

-- The page calls this when the demo boots and when a visitor switches engines: every table back as
-- the demo starts (rows added removed, rows deleted put back), so each visit tells the same story.
-- It runs as its owner (a superuser), and every change it makes reaches Cedar like any other.
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
