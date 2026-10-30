# Cerbos

Cerbos decides on the row's own fields and the principal's roles, so a Cerbos mapping has **no
relationships**: only protected tables, the actions reading and writing a row ask, and the columns
sent as the resource's attributes.

- The type is the resource policy's `resource:` (`resource: document` → table `documents`).
- Store **one rule per policy**, named as you would name its file (`resource/document`,
  `derived_roles/common`). Each rule that covers tables carries their mapping; a derived-roles or
  principal policy has none.
- `read`, `write`, `update`, `delete` name **actions** (`view`, `edit`, `approve`). Cerbos checks
  that the policies have them: `approve:*` covers `approve:big`.
- `attributes` are what the policy reads as `request.resource.attr.<name>`. A column with another
  name: `owner_id as ownerId`.
- The principal: its id is the subject (`authz.configure(subject => 'jwt')`), its roles come from
  the token's `roles_claim` (default `role`; e.g. `authz.configure(roles_claim =>
  'app_metadata.roles')`), its attributes are the token's claims. Scoped policies need
  `authz.configure(scope_claim => 'tenant_id')`.
- Cerbos must store policies in a database with its Admin API on (`storage.driver: sqlite3` or
  `postgres`, `server.adminAPI.enabled: true`) for pg_auth to push them; with policies kept on
  disk or in Cerbos Hub, store the mapping alone (null rules) and name `type`.
- An update is checked on the row before and after: the same action must allow both. A policy
  allowing `approve` only on `status == "open"` refuses the update that sets `status` to
  `approved`. Allow the action on the states it moves rows into (for example a condition on
  `status in ["open", "approved"]` for the fields it reads, with the column grant limited to
  `status`), or do the transition in a privileged function that first checks the caller with
  `authz.can('approve', 'expense:<id>')`: it reads the row's attributes and the caller's roles
  as the policy does.
- The policy's plan for the subject becomes a filter on the attribute columns, so index them as
  you would for RLS. Conditions beyond comparisons, `and`, `or`, `not` and `in` are evaluated per
  row.

`subject => 'jwt'` in these examples trusts the claims PostgREST sets; a client that runs SQL
directly needs `jwt_signed` or a trusted `guc` (see the migrations skill).

## Example: owners and published documents

```sql
select authz.configure(subject => 'jwt');   -- the token's role claim is the principal's role

select authz.set_rules('cerbos', 'resource/document', $cerbos$
apiVersion: api.cerbos.dev/v1
resourcePolicy:
  version: default
  resource: document
  rules:
    - actions: ["view"]
      effect: EFFECT_ALLOW
      roles: ["authenticated"]
      condition:
        match:
          any:
            of:
              - expr: request.resource.attr.owner == request.principal.id
              - expr: request.resource.attr.status == "published"
    - actions: ["edit"]
      effect: EFFECT_ALLOW
      roles: ["authenticated"]
      condition:
        match:
          expr: request.resource.attr.owner == request.principal.id
$cerbos$, mapping => $yaml$
documents:
  read: view
  write: edit
  attributes: [owner, status]
$yaml$);
```

## Example: expenses with camelCase attributes

```yaml
expenses:
  type: expense
  read: view
  write: approve
  attributes: [amount, status, owner_id as ownerId]
```
