# Migrations

pg_auth's whole setup lives in Postgres and changes through migrations: `authz.configure` (who is
asking) and `authz.set_rules` (the engine's rules and the [mapping](mappings.md) of the tables
they decide on). This page is about writing them so they apply cleanly the first time, re-run
safely, and change a deployed setup without opening or closing more than intended.

## What a set_rules call does

```sql
select authz.set_rules(engine, name, rules, mapping => yaml);
```

- Stores a **new version** of the rule `name` for `engine`, unless both the rules and the mapping
  are exactly what is stored already: then it returns the current version and changes nothing.
  Repeating the call is therefore harmless (the rest of a migration, its `create table`, still
  runs once, as migration tools ensure).
- **Applies the mapping at once**, in the migration's transaction: tables named are protected
  (RLS policies that ask pg_auth), relationship tables get their triggers, and rows already in
  them are queued for the engine. Tables the previous mapping named and this one does not are
  unprotected or stop being relationship tables.
- **Checks at commit** that every type, permission, action and relation the mapping names exists
  in the rules. A migration can therefore change the rules and the tables using them in one
  transaction, in any order.
- **The sidecar pushes the rules** to the engine after the commit (Cedar: compiles and validates
  them). Relationships wait until the rules they need are pushed.
- **No mapping keeps the previous one.** `set_rules(engine, name, new_rules)` updates the rules
  and leaves every table as it is. `mapping => ''` removes the mapping (and so the tables'
  setup).
- **Rules kept in the engine itself** (managed in OpenFGA, Cerbos Hub...): pass `null` as the
  rules; only the mapping is stored and nothing is pushed.

Names: OpenFGA has one rule, `model`; SpiceDB one, `schema`; Cerbos one per policy, named like its
file (`resource/document`); Cedar's `schema` rule holds the schema and the mapping, other names
hold policies.

## Order inside a migration

1. Create the tables, their primary keys and the indexes the ids need (`org_id`, `document_id`):
   the mapping is applied when `set_rules` runs, so every table and column must exist by then.
2. Seed rows that must exist before users arrive (the first admin, below), **before** the mapping
   protects the table: afterwards even the table's owner is checked by pg_auth. Rows already in
   a relationship table are delivered to the engine when the mapping first names it.
3. `select authz.configure(...)` if this is the first pg_auth migration (or the subject source
   changes). `subject => 'jwt'` trusts the claims in `request.jwt.claims` without checking a
   signature: right behind PostgREST, which sets them from a verified token, but any client that
   runs SQL directly can set them itself. For those, use `subject => 'jwt_signed'` (with the JWKS)
   or `subject => 'guc'`, where `pg_auth.subject_id` is set by a trusted role only (an API server
   or pooler). Never grant `SET` on it to a role untrusted clients use: they could act as anyone.
4. `select authz.set_rules(...)`: one call per rule.
5. Grants to the app's role (`grant select, insert, update, delete on ... to authenticated`):
   pg_auth decides rows, ordinary privileges still decide tables.

Run it as a role that **owns the tables** and **holds `authz_admin`**: `set_rules` installs
policies as its caller, and only a table's owner can.

```sql
grant authz_admin to postgres;   -- once, by a superuser, for the role your migrations run as
```

## First setup

```sql
-- 20261030120000_documents_sharing.sql
create table public.documents (id bigint generated always as identity primary key,
                               org_id bigint not null, title text not null);
create index on public.documents (org_id);
create table public.memberships (org_id bigint not null, user_id uuid not null, role text not null,
                                 primary key (org_id, user_id));
-- org 1's first admin, before the mapping protects memberships
insert into public.memberships values (1, '00000000-0000-0000-0000-000000000001', 'admin');

select authz.configure(subject => 'jwt');   -- Supabase/PostgREST: the JWT's sub

select authz.set_rules('openfga', 'model', $fga$
model
  schema 1.1
type user
type org
  relations
    define admin: [user]
    define member: [user]
    define can_read: member or admin
    define can_manage: admin
type document
  relations
    define org: [org]
    define viewer: can_read from org
    define editor: can_manage from org
$fga$, mapping => $yaml$
memberships:
  type: org
  id: org_id
  read: can_read
  write: can_manage
  rows: {object: org_id, relation: role, subject: user_id}
documents:
  read: viewer
  write: editor
  insert: org#can_manage from org_id
  relations:
    org: org_id
$yaml$);

-- least privilege: the app updates titles and roles, nothing else (pg_auth also checks a move of
-- org_id as creating the document there). Revoke first: Supabase grants anon and authenticated
-- everything on public tables by default
revoke all on public.documents, public.memberships from anon, authenticated;
grant select, insert, delete on public.documents, public.memberships to authenticated;
grant update (title) on public.documents to authenticated;
grant update (role) on public.memberships to authenticated;
```

Dollar-quote the rules and the mapping with **distinct tags** (`$fga$`, `$yaml$`, `$cedar$`):
a body containing `$$` would otherwise end the string early.

## Changing an existing setup

Each change is a new migration with a full `set_rules` call. Repeat the whole rules text and the
whole mapping: the call stores versions, it does not patch them.

| change | migration |
|---|---|
| change the model or policies only | `set_rules(engine, name, new_rules)`: no `mapping`, the tables stay as they are |
| protect a new table | create the table, then `set_rules` with the same rules and the mapping including it |
| make a table a relationship table | the same: add its `rows`/`relations`. Rows already in it are delivered to the engine |
| change what reading or writing asks | the mapping with the new `read`/`write`; if the permission is new, the rules too (same call) |
| rename a column the mapping uses | `alter table ... rename column` **and** `set_rules` with the mapping naming the new column, in the same migration |
| stop protecting a table | leave it out of the mapping. Its pg_auth policies go, **row level security stays enabled, so the table denies every row** until you add policies. Disable row level security only if the table's grants already keep it from everyone who must not read it (on Supabase, `anon` and `authenticated` have grants by default) |
| switch engine | **in this order, in one migration**: `set_rules(old_engine, name, null, mapping => '')` to clear the old setup, then `set_rules` on the new engine with its rules and the mapping. The other order clears the new setup too (the setup applied is the last engine's). The sidecar must point at the new engine |
| rules managed outside pg_auth | `set_rules(engine, name, null, mapping => ...)`, naming every `type`, `subject_type` and `rows.allowed` |

Mapping changes that only move relationship rows still reach the engine: rows of a table newly
named are delivered, and a table dropped from the mapping stops sending changes (what it sent
stays in the engine; delete it there if it must go).

## The first admin

A relationship table protected with `write` lets only those who already hold the permission add
rows, so nobody can create the first membership of a new org through the API. pg_auth also
*forces* row level security on the tables it protects, so the table's owner is checked like
anyone else. Two ways in:

- **In a migration**: insert the row before the `set_rules` call that protects the table (as in
  the example above). It is delivered to the engine with the rows already there.
- **At runtime** (orgs created by users): a `security definer` function owned by a role with
  `BYPASSRLS`, with a fixed `search_path`, callable only by signed-in users. Owned by the table's
  owner alone it is still checked, and fails.

```sql
create role org_creator nologin bypassrls;   -- owns the function, logs in as nobody
grant select, insert on public.orgs to org_creator;   -- select: for returning id
grant insert on public.memberships to org_creator;
grant usage on schema auth to org_creator;              -- for auth.uid()

create function public.create_org(name text) returns bigint
language plpgsql security definer set search_path = pg_catalog, public, pg_temp as $$
declare org bigint;
begin
  insert into public.orgs (name) values (create_org.name) returning id into org;
  insert into public.memberships values (org, auth.uid(), 'admin');   -- the caller, of the new org only
  return org;
end $$;
alter function public.create_org(text) owner to org_creator;
revoke execute on function public.create_org(text) from public, anon;
grant execute on function public.create_org(text) to authenticated;
```

## Check before merging

Dry-run the migration against a development database with pg_auth (a local Supabase or a Docker
Postgres with the extension), never production: run it inside a transaction, force the
commit-time checks, inspect, roll back. First check that the file has no `begin`, `commit` or
`rollback` of its own: a `commit` inside it would make the dry run real.

```sql
begin;
\i supabase/migrations/20261030120000_documents_sharing.sql
set constraints all immediate;     -- the type/permission checks pg_auth defers to commit
select jsonb_pretty(authz_internal.setup());                  -- what the mapping compiled into
select tablename, policyname from pg_policies where policyname like 'pg_auth%';
rollback;
```

## Check after deploy

```sql
select name, version, pushed_at, error from authz.rules();   -- pushed_at set, error null
select * from authz.doctor();                                -- every check ok, or the fix
```

- `pushed_at` null for long: no sidecar is connected with `PG_AUTH_DATABASE_URL` to push it.
- `error` set: the engine refused the rules (a model it cannot load, a Cerbos policy that does not
  compile). It keeps the version before; store a corrected version.
- Then look at real data as a user: `select * from authz.why('public.documents', '1', as_subject
  => 'user:<uuid>')`.

## Things that surprise people

- **Upgrading pg_auth itself** has no update scripts yet, and the way through is destructive: never
  do it on production without a backup and a window for it.
  Dropping the extension loses its stored rules and versions, and its outbox of relationship
  changes not yet delivered (a revocation still waiting there is lost, so the engine keeps the
  grant). In order:
  1. Stop writes to relationship tables for the duration, and check `authz.doctor()` shows no
     changes waiting.
  2. Clear every engine's mapping (`select authz.set_rules(engine, name, null, mapping => '')`)
     and **commit**: pg_auth's policies and triggers go, row level security stays on, so the
     tables deny every row until step 4. (In the same transaction as the drop it fails: its
     commit-time check is still pending.)
  3. In a new transaction, `drop extension pg_auth` **without** `cascade`. If it still fails, what it lists is the app's
     own (views, functions, policies using `authz.*`): note and recreate them by hand. `cascade`
     would drop them silently, and re-running `set_rules` does not bring them back.
  4. `create extension pg_auth`, then re-run `authz.configure` and every `set_rules` call;
     versions start again at 1.
- **One model per OpenFGA/SpiceDB database**: a second rule name for those engines is refused.
- **Cerbos needs its Admin API** for pg_auth to push policies; otherwise store the mapping alone.
- **Roles in the token** (`roles_claim`) matter for Cerbos and Cedar: set
  `authz.configure(roles_claim => 'app_metadata.roles')` when roles are not in `role`.

## Versions, pushes and checks

```sql
select engine, name, version, mapping, pushed_at, error from authz.rules();   -- the versions in force
select jsonb_pretty(authz_internal.setup());                                  -- what the mappings set up
```

- **Versions.** Every change is a new version in `authz_internal.rules`, with who stored it and when; the
  same rules and mapping again are not one. `authz.rules()` shows the latest of each, `body` and
  `mapping` included. A migration carries the rules and their mapping with the tables they
  protect, and they are reviewed as written.
- **What changes.** A new version sets up what its mapping says: a table it no longer names loses
  its protection or stops being a relationship table, one unchanged is left as it is (its rows are
  not delivered again). A version changing only the mapping is not pushed again: the engine has
  its rules already. Rules stored with no mapping keep the one before, so updating the rules alone
  leaves the tables as they are; `mapping => ''` removes it.
- **Pushed.** The sidecar pushes the latest version within milliseconds (OpenFGA: a new
  authorization model, which it then reads with; SpiceDB: WriteSchema; Cerbos: its Admin API), and
  every backend asks again, since new rules may change any answer. Each push is recorded in
  `pushed_at`. Relationships wait for the rules they need: none is delivered while that engine's
  rules are not pushed.
- **Checked.** The mapping is applied when the rules are stored, so its tables and columns must
  exist by then, and a mapping that is not YAML, or names a key it does not take, fails at once,
  naming its table. A mapping naming a type or permission the rules do not have fails when the
  transaction commits, naming what they have:

  ```text
  ERROR:  the setup names what the rules stored with authz.set_rules do not have:
  public.docs: document has no vewer in the openfga model (it has: editor, org, viewer)
  ```

  That check runs at commit, so a migration can change the rules and the tables using them
  together. A relationship that ends needs, on SpiceDB, a relation that takes it with an
  expiration (and as it is, when the column may be null). One written with a condition needs a
  relation that takes `user with <condition>`, a condition the rules have, and parameters it
  takes. Cerbos policies are checked for their resource and actions (`approve:*` covers
  `approve:big`). What the stored rules do not show, the engine still checks when they are pushed.
- **Refused.** Rules the engine refuses (an undefined relation, a policy that does not compile) are
  kept with its reason in `error`, `authz.doctor()` fails its `rules` check, and the engine keeps
  the version before. Cerbos stores policies it cannot compile and goes on with the ones before:
  the sidecar asks it whether they compile, and puts the versions before back when they do not.
  Store a corrected version to replace it.
- **OpenFGA's DSL** is converted to the API's JSON by the sidecar (schema 1.1: types, relations,
  `[user, user:*, team#member, user with in_region]`, `or`, `and`, `but not`, `from`, conditions);
  modules are refused. JSON is taken as is.
- **Every sidecar follows them.** The engine's id for the rules in force (OpenFGA's model, SpiceDB's
  schema revision) is kept beside them, and every sidecar asks with it from the next request, not
  only the one that pushed them.
- **Cedar** is pushed to no server: the sidecar holding the push compiles the rules in force with the
  new version and validates the policies against the schema (strictly); a version that does not
  is refused with Cedar's reason, the version before kept. Every sidecar then loads the rules in
  force, and the parents delivered (kept in `authz_internal.entity_parents`, read again after each
  delivery). Not taken: a schema with a namespace (a subject `type:id` cannot name `App::User`),
  policy templates, and the request's context.
- **Cerbos** must store policies in a database with its Admin API on
  (`storage.driver: sqlite3` or `postgres`, `server.adminAPI.enabled: true`), and the sidecar needs
  its credentials (`CERBOS_ADMIN_USER`, `CERBOS_ADMIN_PASSWORD`). One `authz.set_rules` row per
  policy, named as you would name its file.

