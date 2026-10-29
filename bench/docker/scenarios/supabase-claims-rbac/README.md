# supabase-claims-rbac

Multi-tenant chat with Supabase's RBAC pattern. Roles live in the JWT: a custom access token hook writes
`app_metadata.orgs = {"<org id>": "<role>"}` into the token; `role_permissions(role, permission)` maps
roles to permissions; policies call `authorize('messages.read', org_id)`.

Tables: `org_members`, `role_permissions`, `channels(org_id)`, `messages(channel_id, org_id, created_at)`.
Roles: guest (channels.read), member (+messages.read), moderator (+messages.delete), admin (+channels.delete).
Subjects: `few_orgs` (orgs 61-64, claims with 4 entries) and `many_orgs` (orgs 1-60, 60 entries; guest
orgs show channels but no messages).

## Indexes and the plan each query should use

| index | serves |
|---|---|
| `messages(created_at desc, id desc)` | `recent`, `join_page` (walk newest first, filter, stop at 20) |
| `messages(org_id, created_at desc, id desc)` | `by_org`, the RLS `org_id = any(...)` filter |
| `messages(channel_id, created_at desc, id desc)` | `feed_a`, `feed_b` and the join column |
| `channels(org_id, id)`, pk | channel policy, `channels` |
| `org_members(org_id, user_id)` + pk `(user_id, org_id)` | live-membership lookup, tuples |

## Variants

- `naive_rls`: `authorize(permission, org_id)` as in the guide (plpgsql, security definer). It takes the
  row's org, so it cannot be hoisted: each message pays a function call, a jsonb path lookup in the claims
  and an index probe on `role_permissions`. Cost grows with claim size (`many_orgs` parses a 60-key object per row).
- `best_practice_rls`: claims parsed once into an array of orgs whose role grants the permission
  (`org_id = any ((select app.orgs_with('messages.read'))::int[])`): index-friendly, close to the baseline.
- `live_membership_rls`: same shape reading `org_members` instead of claims.
- `no_rls_baseline`: the live-membership filter written into each query.
- `pg_auth*`: OpenFGA `org` with a relation per role and `can_read_channels` / `can_read_messages`
  derived from them; `channels` and `messages` protected by `org_id`. One tuple per membership.

## Stale claims

Claims are minted at token issue and only change when the token is refreshed (the hook runs then; Supabase
documents the hook as modifying the access token, so a role change shows up on the next refresh, 1 h by
default). Until then claims-based policies (`naive_rls`, `best_practice_rls`) keep granting what the token
says: a removed moderator reads messages, a newly promoted member does not. This benchmark keeps claims
and tables consistent so results are comparable; to see the problem, delete
`app.org_members` row (user ...02, org 3) and run `many_orgs` through `best_practice_rls`
(still sees org 3) versus `live_membership_rls` and pg_auth after the tuple is deleted (does not).
pg_auth reads live OpenFGA relations, so a revocation takes effect on the next statement (modulo the
sidecar answer cache TTL, 1 s by default), and the JWT only has to carry `sub`.
Claims also grow with memberships (header-size limits) while pg_auth subjects stay one id.

## What to expect

Per-row `authorize()` is the cost: 10-600x on counts and joins. The array form removes it. pg_auth
matches the array forms within noise for these subjects (both have under 1000 readable orgs, so the
listed-ids plan applies); its reused-generic-plan variant and the per-row sidecar path are the weak spots.

## Sources

- Supabase, Custom claims and RBAC (authorize(), role_permissions, custom access token hook, example policies):
  https://supabase.com/docs/guides/api/custom-claims-and-role-based-access-control-rbac
- Supabase, Row Level Security (`select` wrapping, `app_metadata`):
  https://supabase.com/docs/guides/auth/row-level-security
