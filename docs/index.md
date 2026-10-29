---
template: home.html
title: pg_auth — authorization, enforced by Postgres
hide:
  - navigation
  - toc
---

<!-- Rendered by overrides/home.html. This body is not shown on the page but
     still feeds the site search, so keep it accurate. -->

# pg_auth

A PostgreSQL extension that enforces an external authorization engine's decisions as row level
security, for every client: PostgREST, application servers, ad hoc SQL. The engine decides; a
sidecar next to Postgres asks it (OpenFGA or SpiceDB; the protocol is engine-neutral);
pg_auth's planner hook shapes each query so that checking is cheap.

Built with [pgrx](https://github.com/pgcentralfoundation/pgrx) in Rust, for Postgres 15 to 18.

## Quick start

```sql
select authz.configure(subject => 'jwt');   -- the JWT's sub, as PostgREST sets it
select authz.protect('app.documents', 'document', 'id', 'viewer', 'editor');
select * from authz.explain('select * from app.documents');
```

## How a protected scan is checked

| Path | When | Cost |
|---|---|---|
| listed ids | the subject's objects fit the engine's list limit (1000 for OpenFGA) | one engine call per table; `col = ANY(...)` is an index condition |
| batched check | too many to list | an `AuthzBatch` node checks rows in batches as the scan reaches them, in the scan's order |
| permission index | too many to list, with a fresh set in Postgres | rows checked against the set in memory, no engine call |

## Next steps

- [Getting started](usage.md): install, subjects, protecting tables, inspecting, settings
- [How it works](design.md): the planner hook, AuthzBatch, the permission index, plan caching, security
- [Benchmarks](benchmarks.md): every scenario against best-practice RLS
- [Why RLS is slow](research/why-rls-is-slow.md) and the [RLS issues catalogue](research/rls-issues.md)
