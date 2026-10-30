# Mappings

The mapping is the YAML stored with an engine's rules
(`authz.set_rules(engine, name, rules, mapping => ...)`): which tables hold which of the rules'
types, what reading and writing a row asks, and which tables' rows are relationships. It has the
same shape for every engine; [Getting started](../usage.md#3-the-rules-and-their-mapping) has a
complete example on each. This page is the reference.

## Before writing one

1. **Classify each table.** *Protected*: users read or write its rows and the engine decides
   which (it needs `read`). *A relationship table*: its rows grant access (memberships, shares,
   team members, a document's org); it needs `rows` or `relations`, on OpenFGA, SpiceDB and Cedar
   (Cerbos decides on the row's own fields and has none). A relationship entry does not protect
   the table: if the app can write it, protect it too, or nobody but a privileged path may write
   it.
2. **Match each table to a type** in the rules; leave `type` out only when the table's name, made
   singular, is the type (`documents` is `document`; on Cedar, `Document`).
3. **Write rows into direct relations** (`shared_with`, `member`): SpiceDB only accepts writes to
   relations. `read`/`write` may name any relation or permission (`viewer`, or `member` on a team).
4. **Give a child table its parent's id** (`comments: {type: document, id: document_id, read:
   viewer}`), and index it as you would for row level security.

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

## Every key

```yaml
<table>:                         # one entry, or a list of them (a table protected or related twice)
  type: <type>                   # default: the table's name, singular or plural, when the rules have it
  id: <column or expression>     # default: its primary key
  read: <permission>             # what reading a row asks; a protected table needs it
  write: <permission>            # what inserting, updating and deleting asks; without it, denied
  update: <permission>           # and delete: their own permission
  insert: <type>#<permission> from <column>   # inserts checked on a parent
  attributes: [<column>, <column> as <name>]  # the values the rules read
  tree: <table>(<id>, <parent>) from <permission>   # a permission inherited down a tree
  relations:                     # rows that are relationships of this type
    <relation>: <subject column>
    <relation>: {subject: <column>, subject_type: <type>, until: <column>, when: <sql>, ...}
  rows: {object: <column>, relation: <column>, subject: <column>}   # the relation in a column
```

A key it does not know, a type, relation or permission the rules do not have, or a column the
table does not have fails when the transaction commits, naming the table and what the rules have.

- **The id** is a column (the row's own id, or a parent's such as `org_id`), or an IMMUTABLE
  expression over the row (`id: "split_part(name, '/', 1)"`, a storage path's first folder).
  Index it as you would for RLS. Partitions created later are protected too.
- **Several ways to reach a row** (an order visible to its buyer, or to its seller's staff): a
  list of entries, one per way, each with its type. A row is visible when any allows it.
- **An insert checked on a parent:** a table protected by its own id cannot authorize an INSERT,
  the engine has no object yet (`insert: project#create from project_id`).
- **A tree** (folders with `parent_id`): `tree: folders(id, parent_id) from viewer_direct`, with the
  model reading `viewer: viewer_direct or viewer from parent`. The engine lists only direct grants
  and Postgres expands them; triggers keep a closure of the tree.
- **Rules on the row's own attributes** (Cerbos, Cedar): `attributes: [owner_id, status]` sends
  those columns as the resource's attributes, `owner_id as ownerId` under another name. The
  engine's plan for the subject (always, never, or a condition on the attributes) is compiled into
  a filter on those columns: indexes serve it as they serve RLS, and a cached plan serves every
  subject whose plan has the same shape. A condition the engine expresses with more than
  comparisons, `and`, `or`, `not` and `in` is evaluated per row. The principal's roles come from
  the claim `roles_claim` (default `role`; `authz.configure(roles_claim => 'app_metadata.roles')`)
  or, with `subject => 'guc'`, from `pg_auth.subject_roles` (comma separated, superuser-only like
  `pg_auth.subject_id`); its attributes are the token's claims. **Scoped policies** (Cerbos: a
  tenant's overrides, `scope:` in the policy) need the request's scope:
  `authz.configure(scope_claim => 'tenant_id')` sends that claim as Cerbos's scope
  (`pg_auth.subject_scope` for a session subject; `policy_version` for a version other than
  `default`). Without one, a scoped override does not apply; `authz.doctor()` says so.
- **Row state** (`not deleted`, `published`) stays plain SQL: add an ordinary restrictive policy
  next to pg_auth's; it joins the scan's filter.


## How types and subjects are inferred

- **Type**: from the table's name, tried as written, singular (`documents` → `document`,
  `companies` → `company`) and capitalised (`Document`). Exactly one must be a type of the rules;
  otherwise name it. With rules kept in the engine (null body) nothing is inferred.
- **Subject type**: the relation's directly related types, ignoring wildcards (`user:*`) and
  condition suffixes (`user with in_region` counts as `user`). One left: it is used. Several:
  name `subject_type` (and `subject_relation` for a userset).


## Rows that are relationships

An app with no backend changes who may see what by writing rows, through PostgREST like any other
table: a membership, a share, a team. The mapping makes a table's rows relationships in the
engine (OpenFGA, SpiceDB, or Cedar's parents), and pg_auth delivers each change:

```yaml
memberships:
  type: org
  rows: {object: org_id, relation: role, subject: user_id}   # org:<org_id>#<role>@user:<user_id>
documents:
  relations:
    org: org_id                                               # document:<id>#org@org:<org_id>
shares:
  type: document
  relations:
    shared_with: {object: doc_id, subject: user_id}           # document:<doc_id>#shared_with@user:<user_id>
```

- `rows` reads the relation from a column. The relations a row may name are the ones in the rules
  that take the subject; a row naming another fails at once (`check_violation`), where the app sees
  it, instead of the engine refusing it after the commit. `allowed: [reader, writer]` narrows them,
  and names them for rules kept in the engine, which pg_auth cannot read.
- `relations` writes one relation per entry. The object is the entry's `id`, or `object`, else the
  table's single-column primary key.
- The subject's type comes from the rules (the relation's one directly related type). A
  relation's keys say otherwise:

| key | |
|---|---|
| `subject` | the subject's column |
| `subject_type`, `subject_relation` | the subject's type, or a userset (`subject_type: team, subject_relation: member`: the team's members) |
| `subject_id` | a fixed subject, `'*'` for everyone (`user:*`): no subject column |
| `object` | the object's column, when not the entry's id |
| `until` | the relationship ends at the row's timestamp (null: for good) |
| `condition` | written with an OpenFGA condition or SpiceDB caveat, the row giving its parameters (`{name: in_region, with: {allowed: regions}}`) |
| `when` | only rows matching this SQL (one `shares` table for users, teams and the public, one entry per kind) |
| `name` | the declaration's name, when the generated `<type>_<relation>` is not wanted |

- **Write to relations, read permissions.** The rows write plain relations (`admin`, `member`,
  `shared_with`); tables are protected by permissions computed from them (`viewer: shared_with or
  can_read from org`). SpiceDB only takes writes to relations, and the model reads the same in
  OpenFGA.
- **When it shows.** A trigger records each insert, delete and relationship-changing update in
  `authz_internal.outbox` with the write itself, so a rolled-back write never reaches the engine. The
  sidecar delivers it on commit, and every backend asks again: the change shows within tens of
  milliseconds (`compare/relationships.exs`).
- **Grants that end.** `until expires_at`: the sidecar deletes the relationship from the engine
  when it is due, and every backend asks again, as for any revocation: it goes within a few
  hundred milliseconds of its end. An end moved later keeps it longer, one moved earlier ends it at
  the new end (at once if that has passed), a renewal writes it again; a row whose end has passed is no relationship at all.
  SpiceDB also gets the end as the relationship's expiration, so the engine ends it itself if the
  sidecar is down (within its revision window, a few seconds). Its schema says so: `use
  expiration`, and `relation shared_with: user | user with expiration` (`user with expiration`
  alone when the column is `not null`). OpenFGA has no expiry of its own: its grants end when the
  sidecar deletes them, so while the sidecar is down an ended grant stays, like any revocation.
  Ends waiting in the outbox are not changes waiting: `authz.doctor()` counts them apart.
- **Conditions and caveats.** A relationship can carry an OpenFGA condition or a SpiceDB caveat,
  the row giving its values, and each request the rest: a share in some regions, a grant until a
  deadline, an office's IP range.

  ```text
  condition in_region(region: string, allowed: list<string>) {     # OpenFGA
    region in allowed
  }
  type doc
    relations
      define viewer: [user, user with in_region]
  ```

  ```yaml
  shares:
    type: doc
    relations:
      viewer: {object: doc_id, subject: user_id, condition: {name: in_region, with: {allowed: regions}}}
  ```

  ```sql
  -- what every request gives: the time asked, the token's region claim, a header
  select authz.configure(context => '{"current_time": "now", "region": {"claim": "region"},
                                      "client_ip": {"setting": "request.headers", "key": "x-forwarded-for"}}');
  ```

  - A null column gives no value. New values in the row write the relationship again with them.
  - `authz.configure(context => ...)` names what every request gives: `"now"` (the time the
    engine is asked, RFC 3339), `{"claim": "region"}` (the token's claim, `a.b` for a nested one),
    or `{"setting": "request.headers", "key": "x-forwarded-for"}` (a setting, or a key of its
    JSON). A value the request lacks is not sent, and a condition needing it is not met: those
    rows stay hidden.
  - Answers depend on the context, so they are kept per context; one asked with the time is kept
    up to the sidecar's `CACHE_TTL_MS` (1 s), so a grant ends within that of its deadline. The
    permission index is not used while a context is configured: a set is filled for no request
    in particular. Broad subjects are then checked row by row.
  - **The protected row's own values.** A condition can also take values from the row being read
    (a report's level against a reader's clearance): protect the table with them as attributes,
    named as the condition's parameters (`reports: {read: viewer, attributes: [level]}`). What that costs depends on the engine, since an engine's list cannot take a value
    per row:

    | engine | how rows are decided | expect |
    |---|---|---|
    | SpiceDB | one LookupResources per subject (with the request's context): rows granted outright and rows not granted at all are decided in Postgres, by an indexable `id = ANY(...)` filter; only rows granted through a caveat that needs the row's values are checked, each with them | the cost of a list, plus one round trip per row whose grant is conditional. A subject with more than `LIST_LIMIT` objects has every row checked |
    | OpenFGA | ListObjects takes no row's values (and fails on a condition missing one), so every row read is checked with its values | one round trip per row read: fine for reads by key and small pages, slow for scans. Prefer SpiceDB for this shape, or a column the relationship can carry instead |
    | Cerbos | attributes are its own model: plans compile into filters on the columns | no round trip per row |
    | Cedar | partial evaluation leaves conditions on the row: compiled into filters on the columns (an entity attribute among the subject's ancestors becomes `org_id = ANY(...)`, `resource in` the ids below an entity); a part outside comparisons, `in`, `has`, `&&`, `\|\|` and `!` is decided by Cedar for the rows the rest leaves | no round trip per row; the sidecar evaluates a row itself |
- **If the engine is down,** changes wait and are delivered when it is back. A change the engine
  refuses is set aside with its reason, and `authz.doctor()` reports it; the changes after it go
  on.
- **Needs** the sidecar's `PG_AUTH_DATABASE_URL` (as for the permission index), and a unique key
  over the columns, so two rows are never one relationship. Rows already in the table are
  delivered when the mapping first names the table.


## Per engine

- **OpenFGA**: one model per database, the rule named `model`. Usersets as `subject_type: team,
  subject_relation: member`; conditions as `condition: {name, with}`. Grants that end are deleted
  by the sidecar when due (OpenFGA has no expiry of its own).
- **SpiceDB**: one schema, the rule named `schema`. Rows write `relation`s, tables are read by
  `permission`s. A relationship with `until` needs `use expiration` and a relation taking `user
  with expiration` (`user | user with expiration` when the column may be null); SpiceDB then ends
  it itself. Caveats work like OpenFGA conditions.
- **Cerbos**: no relationships. The type is the policy's `resource:`, `read`/`write` name actions,
  `attributes` are what the policy reads as `request.resource.attr.*` (`owner_id as ownerId`). One
  rule per policy, named like its file. The principal's roles come from `roles_claim`, its
  attributes from the token's claims.
- **Cedar**: the rule named `schema` holds the schema and the mapping; other rules hold policies.
  Types are capitalised entity types. A relationship is Cedar's `in`: a row puts its object in its
  subject (`memberships: {type: User, relations: {in: {object: user_id, subject: org_id,
  subject_type: Org}}}`), the parent the policies test with `principal in resource.org`.
  Attribute columns must hold Cedar's types (a `Long` from an integer, an entity from its id).

## Errors and what they mean

| error (prefixed `pg_auth: <engine> mapping: <table>:`) | cause | fix |
|---|---|---|
| `not YAML: ...` | indentation or quoting | YAML flow maps need quotes around values with `:` or `#`, e.g. `when: "role = 'admin'"` |
| `no key \`x\`` | a misspelt key | the message lists the keys an entry takes |
| `name its type (\`type: ...\`): the rules have none called <t>` | the table's name is not a type | add `type:` (the message lists the rules' types) |
| `name its type ...: <a>, <b> would all do` | both `document` and `Document` exist, say | add `type:` |
| `a protected table names what reading a row asks` | `write` or `attributes` without `read` | add `read:` |
| `the rules have no relation <type>#<rel>` | the relation is misspelt or on another type | check the rules; a relationship's type is the *object's* type |
| `<type>#<rel> is computed: rows write a relation the rules assign` | writing to a permission (`viewer`) | write the direct relation it is computed from (`shared_with`) |
| `<type>#<rel> takes user, team#member: say which` | the relation takes several subject types | add `subject_type` (and `subject_relation`) |
| `name its subject's column (\`subject: ...\`) or a fixed id` | a relation without a subject | add `subject:` or `subject_id:` |
| `no relation of <type> takes <subject>` | `rows` whose subject no relation takes | check `subject_type`, or the rules |
| `rows: <type>#<rel> does not take <subject>` | an `allowed` entry the rules do not have for that subject | fix `allowed` |
| `<table> has no single-column primary key: name its id` | no `id`/`object` and no usable primary key | add `id:` (protected) or `object:` (relation) |
| at commit: `the setup names what the rules stored with authz.set_rules do not have: ... <table>: document has no vewer in the openfga model (it has: ...)` | a permission, action or type missing from the rules | fix the name; the message lists what the rules have |
| at commit: a Cedar attribute type error (`Document.size is Long in the cedar schema: column size (text) does not hold one`) | the column's type cannot hold the attribute's | change the column, or the schema |
| `relation "x" does not exist` / `column "y" does not exist` | the table or column is not there yet, or not on `search_path` | create it first, or qualify the name |
