# Setting rules: how usable is it for complex systems?

Status: evaluation and proposal, October 2026. Nothing here is built yet.

Three research passes took the most complex published setups for each engine and wrote them out
as a backend-less app would set them up under pg_auth: the app's tables, the `authz.relate` and
`authz.protect` calls, and a verdict for each. The sources were OpenFGA's sample stores, AuthZed's
schema examples and Cerbos's demo apps and policy docs. None of the calls were run; the verdicts
come from reading pg_auth's code against each model.

This page sums up what they found, compares three ways of setting rules on the hardest cases, and
recommends one.

## What the research found

| Engine | Works as is | Works with a workaround | Not supported |
|---|---|---|---|
| OpenFGA | GitHub orgs and repos, Slack, IoT, custom roles as data, blocklists (`but not`), intersections | Google Drive (wildcard, one `shares` table per type), entitlements AND membership | Conditions (banking limits, temporal access), contextual tuples |
| SpiceDB | Docs-style nested groups, user-defined roles, exclusion and intersection | Google Cloud IAM (wildcards, singleton objects), GitHub (one write permission), multi-tenant superuser | Caveats, expiring relationships |
| Cerbos | Principal policies, clock-only time windows | Expense approval (camelCase attributes, custom actions), list or hierarchy attributes, joined attributes, field-level rules | Scoped policies, request IP, aux data |

What holds up: the rules themselves never need translating. Everything the engines evaluate
(arrows, `but not`, `&`, derived roles, roles as data) works because pg_auth only asks the engine
and does not interpret the model. A plain model with relationship tables is a handful of calls.

Where it strains:

1. **One table, several types.** Real apps keep one `shares` table (`resource_kind`, `grantee_kind`)
   and one `memberships` table holding users, groups and roles. `relate` fixes the object type, the
   subject type and the subject relation per declaration and has no row filter, so each type needs a
   nullable column and its own declaration, and each object type its own table. Hit in most models.
2. **Constants.** `user:*` (public) and singleton objects (`platform:main`) need a column that always
   holds the value, since `relate` takes columns only. A uuid column cannot hold `*`.
3. **One write permission per table.** GitHub needs `push` to update, `delete` to delete and
   `create_repository` to insert; Cerbos policies name actions such as `approve`. `protect` has one
   `write`.
4. **Attribute sources for Cerbos.** Attribute names must equal column names (Cerbos's demos use
   camelCase), and attributes from another table (the owner's department) need a duplicated column.
5. **Context.** OpenFGA conditions, SpiceDB caveats and expiring relationships, Cerbos scopes and
   request IP all need values pg_auth does not send. Conditional grants deny; expiry needs a job that
   deletes rows.
6. **Validation.** A relation name read from a row (roles as data) is checked only when the engine
   refuses it, after the commit; the permissions `protect` names are not checked against the model.
7. **Volume.** The GitHub, Google IAM and Google Drive setups take 8 to 10 `relate` calls plus
   several `protect` calls, spread over a migration, with no single place to review them.

**A security issue, separate from usability, now fixed: Cerbos scoped policies were skipped.** pg_auth
sent no `scope` and no `policyVersion`, so Cerbos answered from the base policy and a scoped
override, deny rules included, never applied: confirmed against a real Cerbos, where a draft a
scoped policy denies came back allowed. The scope now comes from the token (`scope_claim`) or
`pg_auth.subject_scope`, and goes on the principal and the resource; `authz.doctor()` says when
none is configured. `compare/complex.exs` checks a tenant's scoped deny end to end.

## Three ways to set rules

**A. SQL calls, as today.** `authz.relate` and `authz.protect`, one call per table and shape.

**B. One JSON document for the mapping.** `select authz.setup('{...}'::jsonb)` describes every table:
its object type and id, its relationship shapes, its permissions per operation, its Cerbos
attributes. pg_auth compiles it into the same bindings as A, keeps the applied document (formatted,
with history), and returns it from `authz.setup()`.

**C. The engine's own rules, stored in Postgres.** The OpenFGA model, SpiceDB schema or Cerbos
policies, kept as text in `authz.rules` with a version, pushed to the engine by the sidecar, and shown
formatted. This is not a mapping: it says what the rules are, not which table is which object, so it
pairs with A or B.

### The hardest case, each way

A Google-Drive-style app: one `shares` table that shares a document or a folder with a user, a group
or everyone. Folders nest, and the owner edits.

**A, today:**

```sql
-- shares must be split by resource kind, and each grantee kind needs a nullable column
create table doc_shares (doc_id uuid, user_id text, group_id text,
  star text generated always as (case when is_public then '*' end) stored, is_public bool, role text);
create table folder_shares (...);   -- the same again
select authz.relate('doc_shares', object_type => 'doc', object_column => 'doc_id',
  relation_column => 'role', subject_type => 'user', subject_column => 'user_id', name => 'user');
select authz.relate('doc_shares', object_type => 'doc', object_column => 'doc_id',
  relation_column => 'role', subject_type => 'group', subject_column => 'group_id',
  subject_relation => 'member', name => 'group');
select authz.relate('doc_shares', object_type => 'doc', object_column => 'doc_id',
  relation => 'viewer', subject_type => 'user', subject_column => 'star', name => 'public');
-- three more for folder_shares, then the tree, owners, and the protections
select authz.relate('folders', object_type => 'folder', object_column => 'id', relation => 'parent',
  subject_type => 'folder', subject_column => 'parent_id');
select authz.protect('docs', type => 'doc', id => 'id', read => 'can_read', write => 'can_write',
  insert_type => 'folder', insert_id => 'folder_id', insert_permission => 'can_create_file');
```

**B, one document** (the shapes A lacks are marked):

```json
{
  "subject": { "source": "jwt", "claim": "sub" },
  "relationships": [
    { "table": "shares",
      "object":   { "type_from": "resource_kind", "types": ["doc", "folder"], "id": "resource_id" },
      "relation": { "column": "role", "allowed": ["viewer", "editor"] },
      "subject":  { "by": "grantee_kind",
                    "user":   { "type": "user",  "id": "grantee_id" },
                    "group":  { "type": "group", "id": "grantee_id", "relation": "member" },
                    "public": { "type": "user",  "id": "*" } } },
    { "table": "folders", "object": { "type": "folder", "id": "id" }, "relation": "parent",
      "subject": { "type": "folder", "id": "parent_id" } }
  ],
  "protect": [
    { "table": "docs", "type": "doc", "id": "id",
      "select": "can_read", "update": "can_write", "delete": "owner",
      "insert": { "type": "folder", "id": "folder_id", "permission": "can_create_file" } }
  ]
}
```

`type_from`, `by`, constant ids, `allowed` and per-operation permissions are new. The table keeps
its natural shape: one `shares` table, no generated columns.

**C, the rules beside it:**

```sql
select authz.set_rules('openfga', $$
model
  schema 1.1
type folder
  relations
    define parent: [folder]
    define viewer: [user, user:*, group#member] or owner or viewer from parent
...
$$);
```

Then `authz.doctor()` can check every relation the mapping names against the stored model, at once
rather than after a refused write, and `select * from authz.rules` shows what the engine runs.

### Compared

| | A. SQL calls | B. JSON mapping | C. Engine rules in Postgres |
|---|---|---|---|
| Learn | Function arguments | A format of our own | Nothing new: the engine's language |
| Review | Spread over migrations | One document, diffable, kept with history | One text per engine, formatted |
| Natural table shapes (gaps 1 to 3) | Only with new arguments | Yes, it is where they read best | Not its job |
| Validation (gap 6) | After the fact | At apply time, against C | Gives B and A something to check against |
| Rules you already have | Untouched | Untouched | Pasted in as they are |
| Source of truth | The engine | The engine | Postgres, pushed to the engine |

## Recommendation

Keep A as the primitive layer, add B as the way people write and review a setup, and offer C for
teams that want their rules versioned with their schema. In order:

1. **Fix Cerbos scopes** (the security issue above): done.
2. **Grow the primitives** so every shape exists in SQL first, where B compiles to. Done: a `filter`
   and type columns on `relate` (one table, several types), constant ids (`'*'`, singletons),
   allowed relations that fail the write at once, and `on_update` / `on_delete` on `protect`; the
   GitHub and Drive scenarios in `compare/complex.exs` use them; and `attribute_map` names a Cerbos
   attribute otherwise than its column (`ownerId` from `owner`), which the expense scenario uses.
   Still to do: attributes from a joined table or a list, which would put a subquery inside the
   filter the planner hook builds after subqueries are planned, so they need their own design.
3. **Add B**, `authz.setup(jsonb)`: done. It applies a whole mapping in one transaction, fails on
   unknown keys, keeps every version in `authz.setups`, returns the one in force from
   `authz.setup()`, and removes what a new version drops; the Drive scenario in
   `compare/complex.exs` is set up by one document.
4. **Add C**, `authz.set_rules(engine, name, text)`: done. One row per file, as people keep them:
   one OpenFGA model (its DSL, converted by the sidecar) or SpiceDB schema, one row per Cerbos
   policy. Each change is a version in `authz.rules`; the sidecar pushes the latest
   (WriteAuthorizationModel, WriteSchema, Cerbos's Admin API with a database policy store), records
   whether the engine took it, and `authz.doctor()` fails on rules it refused, which keeps the
   version before. Teams that manage rules in the engine keep doing so: C is optional, and when it
   is used Postgres is the source of truth. `compare/rules.exs` checks it on the three engines.
   The setup is checked against the stored rules when a transaction commits: a type, permission,
   relation, subject or Cerbos action they do not have fails it, naming what they have.
5. **Context, last and per engine:** expiring relationships first: done. `expires_column` on
   `relate` ends a row's relationship at its timestamp: the outbox holds its delete until then
   (both engines, within a few hundred milliseconds), and SpiceDB is also given it as an
   expiration, ending it itself when the sidecar is down; `compare/expiry.exs` checks both. Then
   request-level context (time, client IP from a setting) for
   OpenFGA conditions and SpiceDB caveats, then per-row context, which BatchCheck accepts but
   ListObjects does not.

Steps 2 and 3 address the usability gaps most models hit; step 5 is what the banking, temporal
access and IP cases need, and is the largest piece of work.
