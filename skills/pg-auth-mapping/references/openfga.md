# OpenFGA

The mapping names the model's types, relations and permissions (OpenFGA calls both relations;
a *permission* here is any relation defined from others, like `viewer: shared_with or can_read
from org`).

- One model per database: store it as the rule named `model`. A second name is refused.
- Protect with computed relations, write rows into directly assigned ones (`define org: [org]`,
  `define shared_with: [user]`). Writing to a computed relation fails when the rules are stored.
- Conditions: a relation taking `user with in_region` can be written with
  `condition: {name: in_region, with: {allowed: regions}}`; what each request supplies (its time,
  a claim) is `authz.configure(context => ...)`. OpenFGA has no expiry of its own: `until` is
  enforced by the sidecar deleting the tuple when it is due.
- The sidecar converts the DSL (schema 1.1) to OpenFGA's JSON; modules are refused.

`subject => 'jwt'` in these examples trusts the claims PostgREST sets; a client that runs SQL
directly needs `jwt_signed` or a trusted `guc` (see the migrations skill).

## Example: an org's documents, shared with users and teams

```sql
select authz.configure(subject => 'jwt');   -- the request's JWT sub is the user

select authz.set_rules('openfga', 'model', $fga$
model
  schema 1.1
type user
type team
  relations
    define admin: [user]
    define member: [user] or admin
type org
  relations
    define admin: [user]
    define member: [user]
    define can_read: member or admin
    define can_manage: admin
type document
  relations
    define org: [org]
    define shared_with: [user, team#member, user:*]
    define viewer: shared_with or can_read from org
    define editor: can_manage from org
$fga$, mapping => $yaml$
# org:<org_id>#<role>@user:<user_id>; members read memberships, admins change them
memberships:
  type: org
  id: org_id
  read: can_read
  write: can_manage
  rows: {object: org_id, relation: role, subject: user_id}
# team:<team_id>#member@user:<user_id>; members see the team, its admins change it
team_members:
  type: team
  id: team_id
  read: member
  write: admin
  relations:
    member: {subject: user_id, subject_type: user}
# documents are their org's; a new one is checked on the org
documents:
  read: viewer
  write: editor
  insert: org#can_manage from org_id
  relations:
    org: org_id
# one shares table for users, teams and public links, an entry per kind
shares:
  - type: document
    id: doc_id
    read: viewer
    write: editor
    relations:
      shared_with: {subject: user_id, subject_type: user, until: expires_at, when: "kind = 'user'"}
  - type: document
    relations:
      shared_with:
        object: doc_id
        subject: team_id
        subject_type: team
        subject_relation: member
        when: "kind = 'team'"
  - type: document
    relations:
      shared_with: {object: doc_id, subject_id: "*", subject_type: user, when: "kind = 'public'"}
$yaml$);
```

Notes on the example:

- `memberships.role` must be `admin` or `member` (the relations of `org` taking `user`); another
  value fails the insert.
- Every table that writes relationships is also protected: without `read`/`write` on
  `team_members`, anyone with the table grant could add themselves to any team and see what is
  shared with it.
- The first admin of a team or an org is written by a privileged path (see the migrations skill).
- `documents.org_id` decides which org's members see a document. Because `documents` declares
  `insert: org#can_manage from org_id`, pg_auth checks an update of `org_id` as creating the
  document in the new org: an editor cannot move it into an org they do not manage.
- `shared_with` takes three subject forms, so each `shares` entry names `subject_type` (and
  `subject_relation` for the team).
- `comments` protected by their document would be `comments: {type: document, id: document_id,
  read: viewer}`: the engine is asked about the document.
