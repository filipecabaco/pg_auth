# Mapping keys

## Contents

- [A table entry](#a-table-entry)
- [relations](#relations)
- [rows](#rows)
- [How types and subjects are inferred](#how-types-and-subjects-are-inferred)
- [Errors and what they mean](#errors-and-what-they-mean)

The mapping is a YAML map of tables. Each value is one entry, or a list of entries when the
table is protected or related in more than one way. Table names may be schema-qualified
(`app.documents`); unqualified names resolve on the migration's `search_path`.

## A table entry

| key | default | meaning |
|---|---|---|
| `type` | the table's name, singular or plural, as written or capitalised, when the rules have exactly one such type | the rules' type (OpenFGA/SpiceDB type, Cerbos resource kind, Cedar entity type) |
| `id` | the table's single-column primary key | the column the engine knows the row by. A parent's column protects a child table by its parent (`id: document_id`). An IMMUTABLE expression over the row works too (`id: "split_part(name, '/', 1)"`) |
| `read` | — | the permission or relation (Cerbos/Cedar: the action) reading a row asks. Any of `read`, `write`, `update`, `delete`, `insert`, `attributes`, `tree` makes the table protected, and a protected table needs `read` |
| `write` | denied | what insert, update and delete ask |
| `update`, `delete` | `write` | their own permission when it differs. An update is checked on the row before *and* after: the permission must hold for both |
| `insert` | `write` on the row | `<type>#<permission> from <column>`: an insert is checked on a parent object (`org#can_manage from org_id`). Needed on OpenFGA and SpiceDB for a table decided by its own id, since the engine knows nothing about a row that does not exist yet; a table sending `attributes` (Cerbos, Cedar) is decided on the new row's attributes and needs none. Also `{type, permission, from}`; not an action name |
| `attributes` | — | columns sent as the resource's attributes (Cerbos, Cedar; SpiceDB/OpenFGA conditions reading the row): `[owner, status]`, or `[owner_id as owner]` when the rules name it differently. A map `{owner: owner_id}` works too |
| `tree` | — | `<table>(<id>, <parent>) from <permission>`: a permission inherited down a tree the app stores (`folders(id, parent_id) from viewer_direct`, with the model reading `viewer: viewer_direct or viewer from parent`). The engine lists only direct grants; triggers keep a closure of the tree |
| `relations` | — | rows of this table are relationships of this type: `{<relation>: <subject column>}` or `{<relation>: {...}}`, see below |
| `rows` | — | each row is a relationship whose relation is read from a column, see below |

A table entry with only `relations` or `rows` is a relationship table and is **not protected**:
whoever has a write grant on it can write relationships, that is, grant themselves access. Protect
it as well (`read`/`write` may name a relation, e.g. `read: member`), or give app roles no write
grant and write it from a privileged path.

## relations

`<relation>: <subject column>` is short for `<relation>: {subject: <subject column>}`.

| key | meaning |
|---|---|
| `subject` | the subject's column |
| `subject_type` | the subject's type. Required when the relation takes more than one (`[user, team#member]`), or when the rules are kept in the engine |
| `subject_relation` | a userset: `subject_type: team, subject_relation: member` writes `team:<id>#member` |
| `subject_id` | a fixed subject instead of a column: `subject_id: "*"` with `subject_type: user` is everyone (`user:*`) |
| `object` | the object's column, when it is not the entry's `id` |
| `until` | a timestamp column: the relationship ends then (null: for good). SpiceDB needs `use expiration` and a relation taking `user with expiration` |
| `when` | an SQL condition on the row: only matching rows are relationships (one `shares` table with a `kind` column, one entry per kind) |
| `condition` | an OpenFGA condition or SpiceDB caveat, the row giving parameters: `{name: in_region, with: {allowed: regions}}` (parameter: column) |
| `name` | the declaration's name, when `<type>_<relation>` is not wanted (two entries writing the same relation are numbered automatically) |

The object is the entry's `id` when it is a plain column name, else `object`, else the table's
single-column primary key.

## rows

```yaml
memberships:
  type: org
  rows: {object: org_id, relation: role, subject: user_id}
```

writes `org:<org_id>#<role>@user:<user_id>`, the relation taken from the `role` column.

| key | meaning |
|---|---|
| `object` | required: the object's column |
| `relation` | required: the column holding the relation's name |
| `subject`, `subject_type`, `subject_relation` | as in `relations` |
| `allowed` | the relations a row may name. Default: every relation of the type that takes the subject. A row naming another fails at once with `check_violation`. Required in practice when the rules are kept in the engine (pg_auth cannot work them out), and a way to narrow them otherwise |
| `until`, `when`, `condition`, `name` | as in `relations` |

## How types and subjects are inferred

- **Type**: from the table's name, tried as written, singular (`documents` → `document`,
  `companies` → `company`) and capitalised (`Document`). Exactly one must be a type of the rules;
  otherwise name it. With rules kept in the engine (null body) nothing is inferred.
- **Subject type**: the relation's directly related types, ignoring wildcards (`user:*`) and
  condition suffixes (`user with in_region` counts as `user`). One left: it is used. Several:
  name `subject_type` (and `subject_relation` for a userset).

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
