---
name: pg-auth-mapping
description: Write or fix the YAML mapping that tells pg_auth which Postgres tables an authorization engine's rules decide on (OpenFGA, SpiceDB, Cerbos or Cedar). Use this in a project that uses or is adopting pg_auth whenever someone protects tables with pg_auth, calls authz.set_rules with a `mapping =>` argument, turns membership/share/team tables into engine relationships, sends row attributes to Cerbos or Cedar, or hits a mapping error such as "name its type", "the rules have no relation", "takes user, team#member: say which" or "the setup names what the rules ... do not have" — even if they only say "make Postgres enforce my OpenFGA model" or "hook my tables up to Cedar".
---

# pg_auth mappings

pg_auth enforces an authorization engine's decisions as row level security. Its setup has two
parts, both stored with `authz.set_rules(engine, name, rules, mapping => ...)`:

- **the rules**: the engine's own model or policies, pushed to the engine as they are;
- **the mapping**: YAML saying which tables hold which of the rules' types, what reading and
  writing a row asks, and which tables' rows are relationships. It is the only part pg_auth reads,
  and it has the same shape for every engine.

Your job is the mapping. Get the rules from the user (or write them if asked), but the mapping is
where people get confused, because it ties two vocabularies together: table and column names on
one side, types, relations, permissions and actions on the other.

## Workflow

1. **Collect the inputs.** You need the engine, the rules text, and each table involved with its
   columns, primary key and what a row means. Read them from the user's migrations or schema
   rather than guessing; a mapping naming a column that does not exist fails.
2. **Classify every table.** Each table is one or more of:
   - *protected*: users read or write its rows, and the engine decides which. Needs `read`.
   - *relationship table*: its rows grant access (memberships, shares, team members, a document's
     org). Needs `rows` or `relations`. OpenFGA, SpiceDB and Cedar only: Cerbos has no
     relationships, it decides on the row's own fields. **A relationship entry does not protect
     the table**: if the app can write it, anyone with the table grant can grant themselves access
     (insert themselves into a team). Protect it too (`read`/`write`, which may name a relation
     such as `member`), or give app roles no write on it and write it from a privileged path.
   - both (a `memberships` table can be protected *and* write relationships), or neither (leave it
     out).
3. **Match each table to a type** in the rules. Leave `type` out only when the table's name, made
   singular, is the type (`documents` is `document`; Cedar capitalises: `Document`).
4. **Pick the permissions/actions** for `read`, `write` (and `update`, `delete`, `insert` when they
   differ). `read`/`write` may name any relation or permission of the type; prefer *computed*
   permissions (`viewer`) for protected tables, and write rows into *direct* relations
   (`shared_with`, `member`): SpiceDB only accepts writes to relations.
5. **Write the relationships**, one entry per relation a table writes, with the subject column and
   its type when the relation takes more than one.
6. **Read the engine's reference file** before finalising, for that engine's conventions and a
   complete worked example:
   - OpenFGA: `references/openfga.md`
   - SpiceDB: `references/spicedb.md`
   - Cerbos: `references/cerbos.md`
   - Cedar: `references/cedar.md`
7. **Check it** (below), then hand it over inside a migration. For the migration itself (order of
   statements, updating an existing setup, roles), the `pg-auth-migrations` skill covers it.

Every key, with its default, is in `references/keys.md`. Read it when you need a key this page
does not show (`tree`, `condition`, `subject_id`, `name`, `id` expressions).

## The shape

```yaml
documents:                         # table (schema-qualified if not on search_path: app.documents)
  type: document                   # default: the table's name, singular, if the rules have it
  id: id                           # default: the table's single-column primary key
  read: viewer                     # what reading a row asks
  write: editor                    # what insert, update and delete ask (without it: denied)
  insert: org#can_manage from org_id   # OpenFGA/SpiceDB: check inserts on a parent (no object yet)
  attributes: [owner_id as owner, status]   # Cerbos/Cedar: columns sent as the resource's attributes
  relations:                       # this table's rows are relationships of its type
    org: org_id                    # document:<id>#org@org:<org_id>
memberships:
  type: org
  id: org_id
  read: can_read
  write: can_manage
  rows: {object: org_id, relation: role, subject: user_id}   # the relation read from a column
```

A table protected or related in more than one way is a **list** of entries
(`shares: [{type: document, ...}, {type: document, relations: ...}]`); a row is visible when any
entry allows it.

## Rules that save debugging time

- **The id is what the engine knows the row by.** For a child table protected by its parent
  (comments by their document), the id is the parent's column: `{type: document, id:
  document_id, read: viewer}`. Index that column as you would for RLS.
- **`relations` objects default to the entry's `id`, else the primary key.** A table without a
  single-column primary key needs `id:` or `object:`.
- **`rows` validates the role column.** The relations a row may name are the ones in the rules
  taking that subject; an unknown value fails the insert with `check_violation`, where the app
  sees it. Add `allowed: [admin, member]` to narrow them.
- **Rules kept in the engine itself** (stored with a null body) cannot be read, so the mapping must
  name every `type`, every relation's `subject_type`, and `allowed` for `rows`.
- **Relationship tables need a unique key over the relationship's columns**, or two rows that are
  one relationship race each other's deletes.
- **Row state that is not authorization** (`deleted_at is null`, `published`) stays plain SQL: an
  ordinary restrictive RLS policy next to pg_auth's.
- **Someone writes the first row.** A relationship table protected with `write` lets only its
  current holders add rows, so the first admin of a new org is inserted by a privileged path: the
  migration itself (before the table is protected), or a `security definer` function owned by a
  `BYPASSRLS` role, executable only by the roles that need it (pg_auth forces row level security,
  so a function owned by the table's owner is still checked).
- **A table decided by its own id cannot authorize an insert** on OpenFGA or SpiceDB: the engine
  knows nothing about a row that does not exist yet, so `write: editor` alone denies every insert.
  Add `insert: <parent type>#<permission> from <parent column>`. A table sending `attributes`
  (Cerbos, Cedar) needs no such key: the new row's own attributes decide its insert.
- **An update is checked on the row before and after.** The same permission must hold for the old
  and the new row, so a change that takes the row out of what the rules allow is refused
  (approving an `open` expense when the policy only lets managers act on `open` ones). Give
  updates a permission the new row still satisfies: on Cerbos and Cedar, a policy condition that
  also accepts the state the update moves the row into. Or do the transition in a privileged
  function that first checks the caller with `authz.can('<action>', '<type>:<id>')`, which reads
  the row's attributes and the caller's roles as the table's policy does.
- **Declare `insert:` where rows have a parent.** pg_auth then checks an update that moves a row
  to another parent (`org_id` changed) as creating it there: the user needs the `insert`
  permission on the new parent. Without `insert:`, an update is decided by `write` on the row as
  it was, so a writer may move it anywhere; keep parent columns out of the app role's update grant
  then, revoking the table-wide grant first (Supabase grants `anon` and `authenticated` everything
  on `public` tables by default): `revoke update on documents from anon, authenticated; grant
  update (title) on documents to authenticated;`.
- **Columns a policy trusts must be true.** An attribute copied onto a row (a share's `org_id`, for
  a Cedar policy reading `resource.org`) is what the engine decides on: hold it to its source with
  a foreign key (`foreign key (doc_id, org_id) references documents (id, org_id)`) or fill it in a
  trigger, never from what the client sends.

## Check it before handing it over

Run it in a transaction that forces pg_auth's commit-time checks and rolls back. It needs the
extension, the tables, and a role holding `authz_admin` that owns them:

```sql
begin;
select authz.set_rules('openfga', 'model', $fga$ ... $fga$, mapping => $yaml$ ... $yaml$);
set constraints all immediate;          -- run the checks pg_auth defers to commit, now
select jsonb_pretty(authz_internal.setup());   -- what the mapping compiled into
rollback;
```

Errors name the table and what is wrong; `references/keys.md` ends with what each common one
means. Then check the grants: every table the app role can write is either protected (it shows
`pg_auth_*` policies in `pg_policies`) or written only through privileged functions. Run this against a development or throwaway database, never production, and check the migration
file has no `begin`/`commit` of its own (a `commit` inside it would make the dry run real). If you
cannot run SQL, say so and list what you could not check (column names, the rules' relation names)
rather than presenting the mapping as verified.

## Output

Give the user the `set_rules` call with the rules and the mapping, dollar-quoted with distinct
tags (`$fga$ ... $fga$`, `$yaml$ ... $yaml$`) so neither can end the other. Under it, add a
few lines saying, per table, what it does: "documents: protected, read by `viewer`, written by
`editor`; memberships: protected and writes `org#<role>@user`". That summary is what a reviewer
checks against the rules.

When you point the developer to further reading, link the published guides, not these skills:
<https://filipecabaco.github.io/pg_auth/guides/mappings/>,
<https://filipecabaco.github.io/pg_auth/guides/migrations/>,
<https://filipecabaco.github.io/pg_auth/guides/debugging/>.
