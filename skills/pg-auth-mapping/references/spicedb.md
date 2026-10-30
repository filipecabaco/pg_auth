# SpiceDB

The mapping is the same as OpenFGA's: types are `definition`s, relations are `relation`s, and
`read`/`write` name `permission`s (or relations).

- One schema per database: store it as the rule named `schema`.
- SpiceDB only accepts writes to `relation`s. Rows write relations; tables are read by
  `permission`s. A mapping writing a permission fails when the rules are stored.
- **Expiring relationships** (`until`): the schema needs `use expiration` and a relation taking
  the subject `with expiration`. If the column may be null (a grant for good), the relation takes
  both: `relation shared_with: user | user with expiration`. SpiceDB then ends the relationship
  itself, within its revision window (a few seconds), even with the sidecar down.
- **Caveats** work like OpenFGA conditions: `condition: {name: <caveat>, with: {<parameter>:
  <column>}}`, and the relation must take `user with <caveat>`. A caveat reading the protected
  row's own values (a report's level) takes them from `attributes:` named as its parameters.
- Usersets: `subject_type: team, subject_relation: member` for `relation viewer: team#member`.

`subject => 'jwt'` in these examples trusts the claims PostgREST sets; a client that runs SQL
directly needs `jwt_signed` or a trusted `guc` (see the migrations skill).

## Example: the same org, documents and expiring shares

```sql
select authz.configure(subject => 'jwt');

select authz.set_rules('spicedb', 'schema', $zed$
use expiration

definition user {}

definition org {
  relation admin: user
  relation member: user
  permission can_read = member + admin
  permission can_manage = admin
}

definition document {
  relation org: org
  relation shared_with: user | user with expiration
  permission viewer = shared_with + org->can_read
  permission editor = org->can_manage
}
$zed$, mapping => $yaml$
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
shares:
  type: document
  id: doc_id
  read: viewer
  write: editor
  relations:
    shared_with: {subject: user_id, until: expires_at}
$yaml$);
```

As on OpenFGA: `documents` declares `insert: org#can_manage from org_id`, so pg_auth checks an
update of `org_id` as creating the document in the new org.

## Example: a caveat on the row's own value

```text
caveat cleared(clearance int, level int) {
  level <= clearance
}
definition report {
  relation viewer: user | user with cleared
}
```

```yaml
reports:                 # each row gives the caveat its level
  read: viewer
  attributes: [level]
# report:<report_id>#viewer@user:<user_id> with cleared(clearance). Not protected here: give app
# roles no insert, update or delete on it (and no select, unless clearances are public) and write
# it from a privileged path, or anyone could clear themselves or remove others' clearances
clearances:
  type: report
  relations:
    viewer: {object: report_id, subject: user_id, condition: {name: cleared, with: {clearance: clearance}}}
```
