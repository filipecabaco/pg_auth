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

Your app's permissions, enforced by Postgres, with no backend. An authorization engine (OpenFGA,
SpiceDB or Cerbos) decides who may see and change each row, and Postgres enforces that decision
for every client: your browser app through Supabase or PostgREST, an application server, psql.

The home page runs a live demo in the browser tab: Postgres 18 with pg_auth, the three engines and
their sidecars, following one app (a team document editor) in three acts.

## Write the model once. Protect a table in one line

```sql
select authz.configure(subject => 'jwt');   -- the JWT's sub, as Supabase and PostgREST set it
select authz.protect('documents', type => 'document', id => 'id', read => 'viewer', write => 'editor');
```

## Share by writing a row

```sql
select authz.relate('memberships', object_type => 'org', object_column => 'org_id',
                    relation_column => 'role', subject_type => 'user', subject_column => 'user_id');
insert into memberships values (1, 'bob', 'member');   -- bob sees org 1's documents
```

## Ask why a row is hidden

```sql
select * from authz.why('documents', '4', as_subject => 'user:bob');
select * from authz.doctor();
```

## Next steps

- [Getting started](usage.md): install, subjects, protecting tables, granting by writing rows, debugging
- [Performance](performance.md) and [benchmarks](benchmarks.md): what it costs
- [Implementation](design.md): the planner hook, the protocol, security
