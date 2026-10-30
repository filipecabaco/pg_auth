# Engine comparison

Run the same authorization scenarios against several engines and compare them for

1. **correctness**: do the engines give the same answer, and the one the scenario expects?
2. **performance**: latency percentiles and throughput on the same dataset.

Today: `openfga` and `spicedb`. A new engine is one adapter file (see below). These are the
source-of-truth engines pg_auth enforces through its sidecar (`sidecar/query`); pg_auth itself
decides nothing, so it is not an engine here.

## Concepts

- **Scenario** (`lib/scenario.exs`): plain maps. A scenario is a list of *stages*; each stage has a
  `model` (OpenFGA DSL, the canonical source; adapters translate if their engine needs another
  form), `tuples` (written after the model is loaded, they persist when a later stage replaces the
  model), and `ops`.
- **Operations**: `check(subject, relation, object, contextual_tuples, context)`,
  `list_objects(subject, relation, type, ...)`, `list_users(object, relation, filters, ...)`,
  each with an optional expectation: a value, or an error.
- **Loaders** (`lib/loaders/`): `openfga-yaml` (OpenFGA's consolidated suite at the server's version, v1.22.0,
  cached in `compare/.cache/`), `random` (seeded: nested groups, folder tree,
  wildcards, blocklists, `and` / `but not`), `path`
  (hand-written YAML, format below).
- **Engine** (`lib/engine.exs`): the behaviour adapters implement, including `capabilities/1`.
  An operation needing a capability the engine lacks (`:list_objects`, `:conditions`,
  `:contextual_tuples`, `:context`, `:userset_subject`, ...) is reported **unsupported**, never failed.
  A model one engine rejects while another accepts it is also `unsupported` (`model_rejected`).
- **Verdicts**: per operation `agree` / `disagree` (all engines that answered) / `single` (fewer
  than two answered); per engine against the expectation `pass` / `fail` / `no_expectation` /
  `unsupported`. An error answer equals another error answer (messages differ between engines).

## Run

Needs Docker and Elixir (scripts use `Mix.install`). OpenFGA runs `openfga/openfga:v1.22.0` set up as in production: a migrated Postgres datastore (its own container), the check and list caches on, and a connection pool sized to that database. SpiceDB runs `authzed/spicedb:v1.56.2` the same way: a migrated Postgres datastore with `track_commit_timestamp` on (Watch needs it), its caches on (the default), a preshared key, the same pool size, and its HTTP gateway, which the adapter and the sidecar speak. The scenarios' OpenFGA DSL is translated to a SpiceDB schema (`Compare.Engines.Spicedb.Schema`: direct types, usersets, wildcards, `or`/`and`/`but not`, `X from Y`; a relation mixing direct types and rules becomes a relation `<name>__direct` and a permission); conditions are not translated. The OpenFGA
server only takes JSON models, so DSL is converted with the `openfga/cli` image (cached by hash in
`compare/.cache/models/`). Only containers started by the run (label `pg_auth_compare=1`) are removed.

    elixir compare/run.exs --suite openfga-yaml                 # OpenFGA's own suite, all engines
    elixir compare/run.exs --suite random --seed 42 --ops 1000
    elixir compare/run.exs --suite path --path compare/scenarios   # file or directory
    elixir compare/run.exs --engines openfga --suite path          # subset of engines
    elixir compare/run.exs --bench --scale 5 --iters 1000 --concurrency 8

| flag | |
|---|---|
| `--engines a,b` | adapter ids, default `openfga` |
| `--suite` | `openfga-yaml` (default), `random`, `path` |
| `--path` | file or directory for `--suite path` (default `compare/scenarios`) |
| `--seed N`, `--ops N`, `--scale N` | random suite: seed, number of checks (plus ops/10 list_objects, ops/20 list_users), entity multiplier (1 = 200 users, 500 documents) |
| `--filter S` | openfga-yaml: only tests whose name contains S |
| `--bench` | benchmark instead of correctness (suite defaults to `random`; needs a single-stage scenario) |
| `--iters N`, `--list-iters N`, `--warmup N`, `--concurrency N` | timed calls per check kind (default 500), per list kind (iters/5), untimed warmup (50), concurrent workers (8) |

Correctness mode exits 1 when engines disagree. Output: matrix on stdout,
`compare/results/<run>.json` (every operation with each engine's answer) and `<run>.md`.

### Hand-written scenario format

    name: docs-basic
    stages:
      - model: |                       # OpenFGA DSL
          model
            schema 1.1
          type user
          type document
            relations
              define viewer: [user]
        tuples:
          - {object: "document:1", relation: viewer, user: "user:anne"}   # optional: condition: {name, context}
        operations:
          - {op: check, subject: "user:anne", relation: viewer, object: "document:1", expect: true}
          - {op: check, ..., contextual_tuples: [{object, relation, user}], context: {x: 1}}
          - {op: check, ..., expect_error: true}
          - {op: list_objects, subject: "user:anne", relation: viewer, type: document, expect: ["document:1"]}
          - {op: list_users, object: "document:1", relation: viewer, filters: [user], expect: ["user:anne"]}

List answers are compared as sets of `type:id`, `type:id#relation`, `type:*`. Omit `expect` to compare
engines with each other only. Examples: `scenarios/`.

## Adding an engine

Create `compare/lib/engines/<id>.exs` defining `Compare.Engines.<Camelized id>` (`spicedb` ->
`Spicedb`); `--engines <id>` finds it. The callbacks are in `lib/engine.exs`; `engines/spicedb.exs`
is a complete example (a container on a Postgres datastore, the model translated from the DSL,
tuples written over its HTTP API). For parity and the bench (`lib/stack.exs`) an adapter also
provides `fresh_server/1`, `delete_tuples/2`, `staleness_ms/1` (how long after a write its default
reads may still answer from before it), `revocation_ms/1` (the same for an answer it may have
cached), and the sidecar's environment (`Compare.Stack.engine_env/2`).

Adapter rules: state must be safe to share between processes (it is called from tasks);
start long-lived processes unlinked (`Process.unlink`, `Agent.start`) so they outlive the caller;
bind containers to `127.0.0.1` on a free port via `Docker.run/5`; keep names unique.

## What the benchmark measures

Per engine: load time, then for each of `check` / `list_objects` / `list_users`: untimed warmup,
`--iters` sequential calls (latency p50/p95/p99, throughput at concurrency 1) and the same calls
with `--concurrency` workers (throughput under load). Latency is wall time around the adapter
call, including each engine's transport (OpenFGA: HTTP/1.1 + JSON through the Docker port
mapping). Containers share the host with the runner and run one at a time.

## Enforcement parity

`parity.exs` checks the extension end to end: protected tables queried as each subject must show
exactly the rows the engine allows (OpenFGA's ListObjects, SpiceDB's LookupResources).

    elixir compare/parity.exs                                  # seed 1, scale 1, all 200 users + one unknown
    elixir compare/parity.exs --seed 7 --list-limit 20 --subjects 50
    elixir compare/parity.exs --quick --no-build        # while iterating: 40 subjects, no unbatched mode
    elixir compare/parity.exs --modes listed,per_row    # a subset of the modes below
    elixir compare/parity.exs --engine spicedb          # the same against SpiceDB

It runs, on one Docker network: the engine (`--engine openfga`, the default, or `spicedb`; OpenFGA's
ListObjects cap lifted so the reference is complete; SpiceDB's reference pages through
LookupResources at the latest revision), two sidecars built from `sidecar/query/Dockerfile`, and Postgres with pg_auth
(`bench/docker/Dockerfile`). Both images are rebuilt first (`--no-build` skips that). The sidecars
share their sockets with Postgres through a volume:

- `listed.sock`: `LIST_LIMIT` high, every subject gets the listed-ids plan;
- `per_row.sock`: `LIST_LIMIT` = `--list-limit` (default 1), subjects with at least that many
  objects get the per-row check.

Data is the `random` scenario (nested groups, folder tree, wildcards, `but not`, `and`) with numeric
ids, loaded into three protected tables that cover `int8`, `int4` and `text` id columns and a
parent-id binding:

| table | protected by |
|---|---|
| `folders (id int8)` | `folder#viewer` on `id` |
| `documents (id int4)` | `document#can_view` on `id` |
| `comments (id int4, document_id text)` | `document#can_edit` on `document_id` (several per document, one orphan) |

Each subject queries each table in six modes:

| mode | what runs |
|---|---|
| `listed` | through `listed.sock`: listed-ids plan |
| `per_row` | through `per_row.sock`: subjects too broad to list get the AuthzBatch scan (rows checked in batches) |
| `per_row_unbatched` | the same with `pg_auth.batch_check = off`: rows checked one by one |
| `generic_listed_plan` | `plan_cache_mode = force_generic_plan`: planned once for a listed subject, then executed by every subject through `per_row.sock` (subjects too broad to list batch-check the column's values) |
| `generic_per_row_plan` | planned once for a broad subject (an AuthzBatch plan), then executed by every subject through `listed.sock` |
| `indexed` | the permission index: every broad subject's set is filled (the sidecars write into pg_auth's Postgres), then every subject must see its rows with no engine check (`authz.stats()`: no ids checked, index lookups) |

After the modes, `hierarchy` copies the engine's folder tree into `public.folder_tree`, protects
`public.folder_files` with `folder#viewer` declared as a hierarchy over it (the scenario writes
folder grants as `viewer_direct`; `viewer` is `viewer_direct or viewer from parent`), and checks
every subject through both sidecars against the engine's list for `folder#viewer`; through `listed.sock`
the plan must be listed ids, as the expansion applies. The declaration is dropped afterwards.
Then `revocation` deletes a direct folder grant of a broad subject in the engine, waits
until the change feed has marked the index stale (reported in ms) and the engine's own answers
have caught up (OpenFGA's caches expired, 10 s; SpiceDB's quantized reads past the write, 5 s),
and checks the subject sees exactly a consistent list (`HIGHER_CONSISTENCY`, `fully_consistent`),
without the stale set. Then `advice` reads as a broad subject (every set is stale now) and checks that the
next statement's `authz.last_statement_advice()` explains it: ids checked with the engine, and a
set that is not fresh or being filled.

A subject passes when its visible primary keys equal the rows whose id is in the engine's list, and the
plan is the expected one (`authz.explain` for the first three modes, `EXPLAIN EXECUTE` for the
cached plans). When no subject reaches `--list-limit` for a table, `generic_per_row_plan` has
no per-row plan to reuse and that table is reported `skipped`. Output: a table per mode and table
with plan counts, mismatches and latency, in
`results/parity-*.{md,json}`, ending with how long each phase took (the modes run at once); the
run exits 1 on any mismatch. `--quick` checks the plan of every tenth execution in the cached-plan
modes instead of every one. The engine runs as in production
(Postgres datastore, caches on). SpiceDB's default reads may be up to 5 s behind a write, so the stack waits
that long after loading tuples. `--timeout-ms` sets
`pg_auth.sidecar_timeout_ms` (default: the extension's, see below). Only containers, the network and the
volume started by the run are removed, also when it fails.

**Timeouts.** A cached listed-ids plan reused by a broad subject batch-checks every distinct
value of the column: 500 ids take 0.8–1.5 s in OpenFGA. That was past the extension's former
500 ms default `pg_auth.sidecar_timeout_ms` (the query failed closed); the default is now 5 s,
and parity runs with it (`--timeout-ms 500` reproduces the failure). The sidecar now sends a
request's BatchCheck calls `CHECK_CONCURRENCY` (8) at a time, which halved engine-checked scans
in the bench (`nested-rls-chain` comments 2.7 → 1.3 s, one session); an earlier test here, with
8 parity sessions already in flight, had found concurrency slower. Reads by such subjects use
AuthzBatch (one request per batch of rows); parity runs 8
sessions at once, where OpenFGA answers BatchCheck slower than single checks, so its timings for
batched modes are not representative (see `bench/docker/README.md` for single-session numbers).


## Attribute parity

`attributes.exs` checks attribute rules end to end against Cerbos: rows protected by their
attributes (`authz_internal.protect(..., attributes => ...)`) must show each principal exactly what
Cerbos's CheckResources allows for the same rows with the same attributes.

    elixir compare/attributes.exs                                   # seed 1: 400 rows, 80 principals
    elixir compare/attributes.exs --seed 7 --rows 2000 --principals 200 --no-build

It runs Cerbos v0.56.0 with `scenarios/cerbos/item.yaml` (a rule per role, each another shape of
plan: equality and `in` over nullable columns, orderings, a principal's attribute, a list from
the principal, negations, arithmetic that only Cerbos evaluates, a role with no rule), a sidecar,
and Postgres with random rows whose every attribute is sometimes null. Principals are random JWT
claims (`sub`, `role`, a `level`, a list of `tenants`). Each is checked three ways: `compiled` (the
planner's filter), `cached` (one generic plan, reused by every principal on one connection, each in
its own transaction) and `per_row` (`pg_auth.plan_lookup = off`: the plan evaluated per row, or
Cerbos asked per row). On a mismatch it prints the plan. Exits 1 on any mismatch.

`cedar.exs` does the same for Cedar, run by the sidecar: generated rows with nulls in optional and
required columns, entity ids in an integer column, folders, groups and roles from the token, and
for every subject the rows its compiled filter shows against the rows the per-row evaluation of
the same plan shows (`pg_auth.plan_lookup = off`). The sidecar's own tests check that evaluation
against Cedar's decision, row by row.

    elixir compare/cedar.exs                                        # seed 1: 300 rows, 28 subjects
    elixir compare/cedar.exs --no-build --rows 1000 --seed 3

## Relationship tables

`relationships.exs` checks that an application with no backend can grant and revoke access by
writing rows (`authz_internal.relate`), on a real OpenFGA and SpiceDB, with the sidecar's production cache
settings: an org's admin adds a member, a member is refused admin rights and shares outside their
org, shares to a user and to a team, a revocation, and the engine paused while a grant is written
(it waits in the outbox and arrives; an invalid relationship is set aside). Each change is timed
from its commit to the moment the subject sees it.

    elixir compare/relationships.exs                    # OpenFGA, then SpiceDB
    elixir compare/relationships.exs --engine spicedb --no-build

## Grants that end

`expiry.exs` checks relationship tables whose rows end (`authz_internal.relate` `expires_column`), on a
real OpenFGA and SpiceDB, the rules stored in Postgres (`authz.set_rules`): a share shows until its
end and goes after it (timed from the end), an end moved later keeps it and one moved earlier ends
it, a renewal brings it back, the ends waiting in the outbox are not changes waiting, and SpiceDB,
given the end as an expiration, ends it itself with the sidecar paused.

    elixir compare/expiry.exs                           # OpenFGA, then SpiceDB
    elixir compare/expiry.exs --engine spicedb --no-build

## Conditions and caveats

`conditions.exs` checks relationships written with an OpenFGA condition or a SpiceDB caveat
(`authz_internal.relate` `condition`, `condition_context`) and the context each request gives
(`authz.configure(context => ...)`), on a real OpenFGA and SpiceDB, through a sidecar that lists
and one that checks rows (`LIST_LIMIT` 2): a share in some regions shows to a token from one of
them, a token without the claim is not let in, the row's new regions are followed, and a share
until a deadline goes once it passes (timed). A protected table's own values meet the grant's
(reports at or below a reader's clearance, `attributes => array['level']`), and the step reports
how many rows each sidecar asked the engine about: SpiceDB only the rows whose grant is
conditional, OpenFGA every row. The rules are stored in Postgres: one sidecar pushes
them, the other follows.

    elixir compare/conditions.exs                       # OpenFGA, then SpiceDB
    elixir compare/conditions.exs --engine spicedb --no-build

## The demo's rules, and who decides

`notes.exs` loads the demo's rules files (`docker/demo/rules`) unchanged into OpenFGA, SpiceDB
and Cerbos with `authz.set_rules`, and plays the demo's story: the notes in each file protect and
relate the tables, with nothing set up by hand. Then it checks that the engine decides each row,
not Postgres:

- every `pg_auth` policy in `pg_policies` is only a call to `authz.permits` or
  `authz.permits_resource`, with no join or comparison of its own;
- a relationship written straight into the engine shows a row no table in Postgres grants, and
  one deleted straight from the engine hides a row while `memberships` still holds it; on
  Cerbos, a policy put through its Admin API changes what each principal sees;
- with the engine stopped, and then the sidecar stopped, every read and write on a protected
  table fails once the sidecar's 1 s answers have passed.

    elixir compare/notes.exs                            # OpenFGA, SpiceDB, then Cerbos
    elixir compare/notes.exs --engine cerbos --no-build

## The permission index across restarts

`restarts.exs` checks that the change feed resumes from the position it kept in Postgres, on
OpenFGA and SpiceDB, with a broad subject made by a list limit of 1: a second sidecar starting,
and a sidecar restarted, keep the subject's set when nothing changed; a relationship deleted
straight from the engine while no sidecar ran is caught when one starts; and a position the
engine does not serve marks every set stale.

    elixir compare/restarts.exs                         # OpenFGA, then SpiceDB
    elixir compare/restarts.exs --engine spicedb --no-build

## Logical dumps

`dump.exs` sets pg_auth up from the demo's rules with relationship changes still waiting, dumps the
database with `pg_dump`, restores it into another with `pg_restore`, and compares the two: the
settings in force, the protected and relationship tables, the rules, the setup versions, the
waiting changes, the policies and the triggers. A change after the restore must take the next
outbox id. Postgres alone, no engine.

    elixir compare/dump.exs                             # builds the image first
    elixir compare/dump.exs --no-build

## The sidecar over HTTPS

`remote.exs` reaches the sidecar by URL (`pg_auth.sidecar_url`), with a test CA and a certificate
made by openssl, and Postgres's socket pointing nowhere: the demo's story over HTTPS on OpenFGA,
SpiceDB and Cerbos; a secret the sidecar does not hold, an `http://` URL and a certificate another CA
signed each fail the statement; the sidecar refuses an unsigned request, one signed with another
secret, one replayed a minute later and one whose body was changed, and signs its answers for the
request; behind nginx terminating TLS in front of a sidecar on plain HTTP
(`PG_AUTH_TLS_TERMINATED=1`), reads go through, a wrong secret still fails, an answer the proxy
changes fails the statement (more ids than the engine gave, or a Cerbos plan turned into "allow
everything"), and the sidecar's plain port refuses unsigned requests; a sidecar
listening without a secret, with a short one, or without TLS does not start.
It also measures a round trip beyond the engine's time over the socket and over HTTPS.

    elixir compare/remote.exs                           # OpenFGA, SpiceDB, then Cerbos
    elixir compare/remote.exs --engine spicedb --no-build

## History

This harness first compared OpenFGA with the authorization engine pg_auth used to embed
(precomputed permissions in Postgres). At `--scale 10` (22,770 tuples) that engine answered `check`
at 28 ms p50 against OpenFGA's 1.4 ms, and could not list objects at all at request time; it
was removed in favour of asking the engine through the sidecar. The reports are in `results/`.

## Limitations

- OpenFGA is the only reference for expectations beyond the YAML suite; on random scenarios
  "agree" means "same as OpenFGA", not "right".
- Modular models (`module` DSL) are reported unsupported by every engine.
- No tuple deletes or concurrent writers in the neutral format yet.
- Parity covers reads (`select`) only; writes and the `write` permission are covered by the
  extension's unit tests with the fake sidecar.
- Bench numbers come from one machine, Docker-in-VM networking, a single run each; compare
  engines within a run, not across runs.
