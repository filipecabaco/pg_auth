# Cedar

Cedar runs inside pg_auth's sidecar: no engine server. Its rules are a schema and policies.

- **The rule named `schema`** holds the Cedar schema and carries the mapping. Every other rule
  name holds policies (`set_rules('cedar', 'documents', $cedar$ permit ... $cedar$)`, no
  mapping). No namespaces, templates or request context.
- **Types** are the schema's entity types, capitalised: a table `documents` maps to `Document`
  on its own, other names need `type:`. The subject is configured with
  `authz.configure(subject => 'jwt', subject_type => 'User')`.
- **Actions** are what `read`/`write` name (`view`, `edit`).
- **Relationships are Cedar's `in`**: a row puts its *object* entity in its *subject* entity, the
  parent the policies test with `principal in resource.org` or `resource in folder`. The schema
  must allow it (`entity User in [Org]`). So for memberships the object is the user and the
  subject the org:

  ```yaml
  memberships:
    type: User
    relations:
      in: {object: user_id, subject: org_id, subject_type: Org}
  ```

  Parents can chain (`entity Folder in [Folder]`, `folders: {relations: {in: {object: id,
  subject: parent_id, subject_type: Folder}}}`), and a role column picks a parent group with
  `when`.
- **Attributes** are the entity's, read as `resource.<name>`: `attributes: [owner_id as owner,
  status]`. The column's type must hold Cedar's: a `Long` from an integer column, a `String` from
  text, an entity reference (`org: Org`) from any column holding its id. A mismatch fails at
  commit. Optional attributes (`status?: String`) may be null; a required one that is null denies
  the row.
- The principal's own attributes come from the token's claims; its roles (when the schema lets the
  principal be `in` a `Role`) from `roles_claim`.

`subject => 'jwt'` in these examples trusts the claims PostgREST sets; a client that runs SQL
directly needs `jwt_signed` or a trusted `guc` (see the migrations skill).

## Example: orgs, their admins, documents and expiring shares

```sql
select authz.configure(subject => 'jwt', subject_type => 'User');

select authz.set_rules('cedar', 'schema', $cedar$
entity Org;
entity OrgAdmins;
entity User in [Org, OrgAdmins, Document];
entity Document { org: Org, admins: OrgAdmins };
action read appliesTo { principal: User, resource: Org };
action manage appliesTo { principal: User, resource: OrgAdmins };
action view, edit appliesTo { principal: User, resource: Document };
$cedar$, mapping => $yaml$
# a membership puts its user in the org, and in OrgAdmins when its role is admin
memberships:
  - {type: Org, id: org_id, read: read}
  - {type: OrgAdmins, id: org_id, read: manage, write: manage}
  - type: User
    relations:
      in: {object: user_id, subject: org_id, subject_type: Org}
  - type: User
    relations:
      in:
        object: user_id
        subject: org_id
        subject_type: OrgAdmins
        when: "role = 'admin'"
documents:
  read: view
  write: edit
  attributes: [org_id as org, org_id as admins]
# a share puts its user in the document, until expires_at
shares:
  - type: Document
    id: doc_id
    read: view
    write: edit
    attributes: [org_id as org, org_id as admins]
  - type: User
    relations:
      in:
        object: user_id
        subject: doc_id
        subject_type: Document
        until: expires_at
$yaml$);

select authz.set_rules('cedar', 'documents', $cedar$
permit (principal, action == Action::"read", resource is Org)
  when { principal in resource };
permit (principal, action == Action::"manage", resource is OrgAdmins)
  when { principal in resource };
permit (principal, action == Action::"view", resource is Document)
  when { principal in resource.org };
permit (principal, action == Action::"view", resource is Document)
  when { principal in resource };
permit (principal, action == Action::"edit", resource is Document)
  when { principal in resource.admins };
$cedar$);
```

Notes on the example:

- `memberships` is protected twice (as `Org` and as `OrgAdmins`) and relates twice: a list of
  four entries.
- `org` and `admins` both come from `org_id`: the org entity and its admins group share the id.
- A `shares` row carries its document's `org_id`, and the `edit` check trusts it: a share naming
  an org the user administers would pass for any document. Hold it to the document with a key,
  so it cannot disagree: `unique (id, org_id)` on `documents`, and on `shares`
  `foreign key (doc_id, org_id) references documents (id, org_id)` (the demo also fills it with a
  trigger from `documents`).
