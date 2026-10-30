---
name: pg-auth-migrations
description: In a project that uses or is adopting pg_auth, write the database migrations that set up and change its authorization: storing an engine's rules and their table mapping with authz.set_rules, updating a model or policy, adding or removing a protected table, renaming columns a mapping uses, bootstrapping the first admin, and checking the result after deploy. Use this whenever someone writes or reviews a Supabase/Postgres migration that touches pg_auth (authz.set_rules, authz.configure, a mapping, OpenFGA/SpiceDB/Cerbos/Cedar rules), plans how to roll out or change permissions in production, or asks why a migration with pg_auth failed at commit, even if they just say "add sharing to my app" or "change who can edit documents".
---

# pg_auth migrations

pg_auth's whole setup lives in Postgres and is changed by migrations: `authz.configure` (who is
asking) and `authz.set_rules` (the engine's rules and the YAML mapping of the tables they decide
on). This skill is about writing those migrations so they apply cleanly the first time, re-run
safely, and change an existing setup without opening or closing more than intended.

For the mapping itself (which keys, per engine), use the `pg-auth-mapping` skill. For a deployed
setup that misbehaves, use `pg-auth-debug`.

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

-- the parent column stays out of update: an editor could otherwise move a document to another org.
-- Revoke first: Supabase grants anon and authenticated everything on public tables by default
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

If the dry run cannot be done, say which checks were skipped instead of calling the migration
verified.

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
  do it without the user's explicit go-ahead, and never on production without a backup.
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

When you point the developer to further reading, link the published guides, not these skills:
<https://filipecabaco.github.io/pg_auth/guides/mappings/>,
<https://filipecabaco.github.io/pg_auth/guides/migrations/>,
<https://filipecabaco.github.io/pg_auth/guides/debugging/>.
