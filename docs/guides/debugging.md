# Debugging

pg_auth answers every question about a row through functions in Postgres, so start there. The
statements below run in `psql` or any SQL editor; `as_subject` and `pg_auth.as_subject` need an
administrator.

## Start with these two

```sql
select * from authz.doctor();            -- every part of the setup, each with the statement that fixes it
select * from authz.current_subject();   -- who pg_auth thinks is asking, and why
```

`doctor` checks the subject source, the sidecar, the rules (pushed or refused, and whether the
setup matches them), the relationship tables' waiting and refused changes, and the protected
tables. Fix what it fails first: most row problems are a
setup problem it already names.

`current_subject` run *as the app would* (inside the request's transaction, with its role and
claims) shows the subject pg_auth resolved. **No subject means no rows.** With Supabase/PostgREST
reproduce a request like this:

```sql
begin;
select set_config('request.jwt.claims', '{"sub": "<user uuid>", "role": "authenticated"}', true);
set local role authenticated;
select * from authz.current_subject();
select * from public.documents;          -- what that user sees
rollback;
```

## By symptom

### A user sees no rows, or too few

```sql
select * from authz.why('public.documents', '<row id>', as_subject => 'user:<id>', as_role => 'authenticated');
```

`as_subject` is `<subject type>:<id>`, with the type `authz.configure` set: `user:<id>` by
default, `User:<id>` on Cedar (types are case-sensitive; the wrong case checks a principal that
belongs to nothing). It walks the row: the subject, row level security, the table privilege, each
policy, the engine's verdict, and the final answer. Read the first failing step:

- **subject missing**: `authz.configure` does not match how the app sends identity (claim name,
  `claims_setting`, `subject => 'guc'` without `pg_auth.subject_id`).
- **privilege**: the role lacks `grant select` on the table. pg_auth decides rows, not tables.
- **engine denies**: the relationship or policy is not there. Check the rows that should grant it,
  and the engine's own answer: `select authz.check('user:<id>', 'viewer', 'document:<id>')`.
- **another policy** (a hand-written restrictive one) removes it.

For Cerbos and Cedar, pass the roles the token would carry: `subject_roles => array['editor']`.

### An insert or update is refused

- `new row violates row-level security policy`: the subject lacks the `write` (or `insert`)
  permission on the row's object. With `insert: org#can_manage from org_id`, the check is on the
  *org*, not the new row. `authz.why` on an existing row with the same parent shows the verdict.
- Every insert refused on OpenFGA/SpiceDB, even for admins: the table is decided by its own id and
  has no `insert:` key, and the engine knows nothing about a row that does not exist yet.
- An update refused although the user may change the row: updates are checked on the row before
  *and* after, so a change that takes the row out of what the rules allow (approving an `open`
  expense under a policy that only allows `open` ones) fails its check.
- `pg_auth: moving a row of <table> to <type>:<id> needs <permission> on it`: an update changed
  the parent column of a table declaring `insert:`, and the user may not create rows under the
  new parent.
- `authz.check` on a Cerbos or Cedar table answers with the row's attributes; give the roles the
  subject's token would carry: `authz.check('user:anne', 'view', 'expense:7', subject_roles =>
  array['manager'])`.
- `check_violation` on a relationship table: the role column names a relation the rules do not
  have for that subject (`owner` when the model has `admin` and `member`). The error lists them.
- The very first row of a new org: nobody holds the permission yet. Seed it in a migration before
  the table is protected, or write it from a function owned by a `BYPASSRLS` role.

### Every query on a protected table fails

Statements fail rather than guess when the sidecar or the engine cannot answer:

- `pg_auth sidecar at /tmp/pg_auth.sock: No such file or directory` (or `Connection
  refused`; the path is `pg_auth.sidecar_socket`): the sidecar is not running, or `pg_auth.sidecar_socket` (or `sidecar_url`) points
  elsewhere. `select * from authz.status()` shows what Postgres uses.
- `the sidecar did not answer within pg_auth.sidecar_timeout_ms (5000 ms)`: the sidecar is up
  but stuck, usually waiting on the engine.
- The engine down: the sidecar answers with the engine's error after `ENGINE_TIMEOUT_MS` (4 s).
  Restart or reach the engine; relationship rows written meanwhile wait in the outbox.

### A grant written as a row does not show

- `select * from authz.doctor()`: the `relationship tables` check counts changes waiting, and
  changes the engine refused, with its reason and the query listing them. A refused change is set
  aside; the ones after it go on.
- No sidecar with `PG_AUTH_DATABASE_URL`: nothing delivers. Every relationship waits.
- The rules the relationship needs are not pushed yet (`authz.rules()` `pushed_at` null): it
  waits for them.
- Written directly in the engine (not through a table): it shows once the engine's own reads see
  it, up to about 10 s on OpenFGA with caches and 5 s on SpiceDB.

### Rules are not pushed, or refused

```sql
select engine, name, version, pushed_at, error from authz.rules();
```

- `pushed_at` null and no `error`: no sidecar has pushed them (not running, or without
  `PG_AUTH_DATABASE_URL`).
- `error` set: the engine refused this version (OpenFGA cannot load the model, a Cerbos policy does
  not compile, Cedar's validation fails). The engine keeps the version before; store a fixed one.
- A migration failing *at commit* with `the setup names what the rules ... do not have`: the
  mapping names a type, permission or relation missing from the rules. The message lists what
  the rules have.

### A protected query is slow

```sql
select * from authz.explain('select * from public.documents order by created_at desc limit 20');
select * from authz.last_statement_advice();   -- after running the slow statement
select * from authz.stats();                    -- this backend's round trips and engine time
```

`explain` says, per protected table, how pg_auth decided rows (listed ids, a batched check, the
permission index) and why, with advice. Common causes: the `id` column has no index; a subject
with access to a very large share of rows on its first statement (the permission index fills,
later statements are fast); a condition reading the row's values on OpenFGA, which checks every
row (SpiceDB, Cerbos and Cedar filter instead).

## Everything else that helps

```sql
set local pg_auth.as_subject = 'user:anne';   -- see the data as anne does (superuser only)
select authz.can('viewer', 'document:42');                 -- the current subject
select authz.check('user:anne', 'viewer', 'document:42');  -- any subject (administrators)
select * from authz.lint();           -- hand-written RLS policies scored against common issues
```

`pg_auth.warn_ms` (off by default) raises a WARNING with the advice when a statement's engine
time reaches it, for clients that never call `last_statement_advice()`.
