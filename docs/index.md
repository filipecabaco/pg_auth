---
template: home.html
title: "pg_auth: your app's permissions, enforced by Postgres"
hide:
  - navigation
  - toc
---

<!-- Rendered by overrides/home.html. This body is not shown on the page but still feeds the site
     search and the menu's table of contents on a phone, so keep it accurate, and keep each
     heading's id the id of its section on the page. -->

# pg_auth

Your app's permissions, enforced by Postgres, with no backend. An authorization engine (OpenFGA,
SpiceDB, Cerbos, or Cedar, which the sidecar runs itself) decides who may see and change each row, and Postgres enforces that decision
for every client: your browser app through Supabase or PostgREST, an application server, psql.

The home page runs a live demo in the browser tab: Postgres 18 with pg_auth and the four engines.
Anne and Bob use the same app; Bob sees nothing until Anne adds him to her organisation by inserting
one row. It runs where you configure Postgres yourself: the extension loads when Postgres starts,
which RDS, Cloud SQL, Neon and hosted Supabase do not allow. The sidecar runs beside Postgres, or on another
host over HTTPS with signed requests.

## How it works {#how}

Your app queries Postgres; pg_auth asks the sidecar, which asks the engine, and Postgres shows only
the rows the engine allows. Grants written as rows reach the engine through an outbox.

## Write each rule once, not in every table {#model}

```sql
select authz.configure(subject => 'jwt');   -- the JWT's sub, as Supabase and PostgREST set it
select authz.set_rules('openfga', 'model', $fga$ ... $fga$, mapping => $yaml$
memberships:
  type: org
  rows: {object: org_id, relation: role, subject: user_id}
documents:
  read: viewer
  write: editor
  relations:
    org: org_id
$yaml$);
-- share by writing a row: bob sees org 1's documents
insert into memberships values (1, 'bob', 'member');
```

The rules stay in the engine's own syntax, kept in Postgres with your migrations; the mapping
beside them protects and relates the tables.

## Ask why a row is hidden {#why}

```sql
select * from authz.why('documents', '4', as_subject => 'user:bob');
select * from authz.doctor();
```

## The row level security mistakes you cannot make {#mistakes}

Forgotten policies, views that leak, recursion, leaky functions, identity left on a pooled
connection: each checked in the RLS issues catalogue.

## What to expect {#cost}

About the time of a best-practice policy overall (the benchmarks); grants written as rows show
within tens of milliseconds, and a share with an end date (`until` in its mapping) disappears when
it ends; queries fail when the engine is down; the sidecar runs beside Postgres or over HTTPS;
experimental.

## Pick the engine that fits your rules {#engines}

OpenFGA and SpiceDB decide on relationships (with conditions and caveats); Cerbos on the row's
fields and the user's roles.

## Try it on your own Postgres {#install}

- [Getting started](usage.md): install, subjects, protecting tables, granting by writing rows, debugging
- [Performance](https://github.com/filipecabaco/pg_auth/blob/main/docs/performance.md) and [benchmarks](benchmarks.md): what it costs
- [Implementation](design.md): the planner hook, the protocol, security
