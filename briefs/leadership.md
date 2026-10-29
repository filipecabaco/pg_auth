# pg_auth: a brief for leadership

October 2026. Status: experimental, pre-release, no license chosen.
Site and live demo: https://filipecabaco.github.io/pg_auth/

## In one paragraph

pg_auth is a Postgres extension and a small sidecar. An authorization engine (OpenFGA, SpiceDB or
Cerbos) holds an app's access rules, and Postgres enforces them on every query as row level
security. An app that talks to Postgres directly, through PostgREST or a client library, gets
sharing, teams, roles and per-row rules without an API server and without hand-written policies.
A table is protected with one call; access is granted by inserting a row. It works today on
Postgres you run yourself. It does not run on hosted Supabase, because it needs things the
platform does not offer customers (below).

## The problem it addresses

Apps with no backend put authorization in row level security. That works for "users see their own
rows", and gets hard quickly after that:

- Sharing, teams, organisations and inherited folders become helper functions and a policy per
  operation, repeated on every table, kept in sync by hand.
- Mistakes are silent: a wrong policy hides rows or shows them, with no error. We catalogued 35
  real RLS problems people hit (leaking views, policy recursion, updates that change 0 rows,
  partitions queried directly, per-row function cost) and built a benchmark scenario for each.
- Teams that need more move authorization into a backend service, which is exactly what a
  no-backend app was trying to avoid.

## What it does

- **One line per table.** `authz.protect('documents', type => 'document', id => 'id', read => 'viewer')`
  installs the policies; the planner turns the engine's answer into an indexable filter.
- **Grants are rows.** Inserting into a memberships table delivers the relationship to the engine;
  the subject sees the change 4 to 34 ms after commit (measured against OpenFGA and SpiceDB).
- **It explains itself.** `authz.why` says why a row is hidden; `authz.doctor` checks the setup and
  prints the fix.
- **It fails closed.** If the engine cannot answer, queries on protected tables fail rather than
  show rows.

## Evidence

| What | Result |
|---|---|
| Query cost vs best-practice RLS | 0.75x overall over 489 query × subject pairs (35 scenarios); from 0.22x to 1.71x per scenario. Slower on sub-millisecond statements, where its planning work shows |
| Correctness | Parity suites compare every scenario's rows with each engine's own answers, including cached plans reused across users |
| Tests | Extension tests on Postgres 15 to 18 in CI; 96.8% combined line coverage; the coverage work found two bugs, both failing closed |
| Grant latency | 4 to 34 ms from commit to visible |
| Live demo | Postgres 18, pg_auth and all three engines booting in a browser tab |

User tests with three personas (a Supabase app builder, an engineer already running OpenFGA, a
newer developer on a phone) after a round of fixes: all three understood what it does within
seconds; the demo worked end to end; every claim the engineer probed held. None would adopt it
today, for the same three reasons: **no license, hosted Supabase cannot run it, and managed
Postgres in general does not allow it.**

## Why it cannot run on hosted Supabase today

1. **`shared_preload_libraries`.** pg_auth hooks the planner and executor, so it must be preloaded
   when Postgres starts. Customers cannot add libraries there.
2. **A sidecar on the same host.** Postgres talks to the sidecar over a Unix socket; the sidecar
   talks to the engine. Customers cannot run a process next to their database.
3. **An engine.** Someone has to run OpenFGA, SpiceDB or Cerbos, reachable from the sidecar, with
   its own availability: when it is down, protected queries fail.

## Options

| Option | What it takes | What customers get |
|---|---|---|
| A. Managed | The platform preloads pg_auth, runs the sidecar, and offers a managed engine | Authorization for no-backend apps as a product feature; the most work and the most operational surface |
| B. Bring your engine | The platform preloads pg_auth and runs the sidecar; the customer points it at their own engine | Teams already on OpenFGA, SpiceDB or Cerbos enforce it in Postgres; less to operate, engine availability stays the customer's |
| C. Self-hosted only | Nothing from the platform | Today's state: anyone running their own Postgres can use it |

Risks to weigh for A or B: availability coupling to the engine (fail closed is the safe default but
turns an engine outage into a data-path outage), a security review of planner hooks (unsafe Rust is
confined to two modules), and support for a new class of failure ("my query failed because the
engine was slow").

## What I am asking for

1. **A license decision**, so outside teams can try it at all.
2. **A yes or no on exploring option B** with one design partner, to learn whether the demand is
   real before committing to a managed engine.
3. **Time for a security review** of the extension's hooks and the sidecar, if we explore further.

## Where to look

- Live demo and overview: https://filipecabaco.github.io/pg_auth/
- Benchmarks and method: https://filipecabaco.github.io/pg_auth/benchmarks/
- How it is tested: https://filipecabaco.github.io/pg_auth/testing/
- The RLS issues catalogue: https://filipecabaco.github.io/pg_auth/research/rls-issues/
- Source: https://github.com/filipecabaco/pg_auth
