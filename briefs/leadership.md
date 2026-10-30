# pg_auth: a brief for leadership

October 2026. Status: experimental, pre-release, no license chosen, in a personal repository.
Site and live demo: https://filipecabaco.github.io/pg_auth/

## In one paragraph

pg_auth is a Postgres extension and a small sidecar. An authorization engine (OpenFGA, SpiceDB or
Cerbos) holds an app's access rules, and Postgres enforces them on every query as row level
security. An app that talks to Postgres directly, through PostgREST or a client library, gets
sharing, teams, roles and per-row rules without an API server and without hand-written policies.
The setup is the engine's own rules with a comment per table they protect; access is granted by
inserting a row. It works today on Postgres you run yourself. It does not run on hosted Supabase:
it needs the extension preloaded, which the platform does not offer customers (below).

## The problem it addresses

Apps with no backend put authorization in row level security. That works for "users see their own
rows", and gets hard quickly after that:

- Sharing, teams, organisations and inherited folders become helper functions and a policy per
  operation, repeated on every table, kept in sync by hand.
- Mistakes are silent: a wrong policy hides rows or shows them, with no error. A draft catalogue
  collects 35 RLS problems from public reports (leaking views, policy recursion, updates that change
  0 rows, partitions queried directly, per-row function cost), each with a benchmark scenario. It
  shows the problems exist; it does not show how many projects have them.
- Teams that need more move authorization into a backend service, which is exactly what a
  no-backend app was trying to avoid.

## What it does

- **A comment per table.** `# pg_auth: protects documents (id): read viewer`, in the engine's own
  rules, installs the policies; the planner turns the engine's answer into an indexable filter.
- **Grants are rows.** Inserting into a memberships table delivers the relationship to the engine;
  the change showed 4 to 34 ms after commit in tests against OpenFGA and SpiceDB.
- **It explains itself.** `authz.why` says why a row is hidden; `authz.doctor` checks the setup and
  prints the fix.
- **It fails closed.** If the engine or the sidecar cannot answer, queries on protected tables fail
  rather than show rows.

## Evidence

| What | Result |
|---|---|
| Query cost vs best-practice RLS | 0.80x with OpenFGA, 0.78x with SpiceDB: geometric means over 489 query and subject pairs in 35 scenarios, faster in 201 and 192 of them. Per scenario from 0.21x (a folder tree) to 1.70x (a policy on an unindexed column). Single statements on one host, sidecar on a Unix socket: no concurrency, throughput or engine cost measured |
| Correctness | Parity suites compare every subject's rows with each engine's own answers, including plans reused across users |
| Tests | 103 extension tests on Postgres 15 to 18 in CI; end-to-end suites against real engines run locally |
| Grant latency | 4 to 34 ms from commit to visible, one sidecar |
| Sidecar over HTTPS | Requests and answers signed; about 0.2 ms per round trip more than the socket, on one host, network not included |
| Request rate, under concurrency | PostgREST-shaped requests by 1,000 random users, Postgres on 2 CPUs, almost every request asking the sidecar: 0.59x to 0.67x of best-practice RLS's throughput on the socket, 0.36x to 0.50x over HTTPS on the same host. 6.2 ms of added round trip costs 5% of throughput at 16 clients (a waiting backend uses no CPU), and is added to each request's latency once |
| Live demo | Postgres 18, pg_auth and all three engines booting in a browser tab |

Persona reviews were run by AI agents playing users (a Supabase app builder, a self-hosted team, a
security reviewer, a managed-Postgres team, a Postgres image owner). They are not user research:
they found documentation gaps and bugs, not demand.

## What this does not have yet

- **Evidence of demand:** no users or design partners. Sizing it needs fleet data (projects with
  many policies, `security definer` helpers in policies, the RLS advisor's warnings) and support
  and sales requests for relationship-based access.
- **The setup a platform would run, measured on real hosts:** request rate and latency are
  measured on one host with network delay added; not yet on two hosts, nor with the engine away
  from the sidecar. Postgres CPU limits throughput: 530 µs per request against RLS's 184,
  most of it pg_auth's own work (parsing the sidecar's answers, reading the permission index), which
  a profile has located; HTTPS adds 3% at low concurrency and 36% at 64 clients on 2 CPUs.
- **Operations a platform needs:** extension upgrade scripts, which start with the first release
  (there is nothing installed to upgrade before it), checked in CI against a fresh install. (Logical backups keep pg_auth's
  configuration, a statement waiting on the sidecar gives way to a cancel, and the sidecar's own
  connection to Postgres is encrypted and checked, as of October 30.)
- **A license, and an owner** for the code.

## Why it cannot run on hosted Supabase today

1. **`shared_preload_libraries`.** pg_auth hooks the planner and executor, so it must be loaded
   when a session starts. Customers cannot add libraries there.
2. **A sidecar to run.** The sidecar can run on another host, reached over HTTPS with signed
   requests and answers, so it no longer needs the database host. Someone still has to run it
   near the database. Supabase Compute could, once pg_auth is in the image; its private alpha
   does not say yet whether a service can be reached privately or kept from suspending.
3. **An engine.** Someone runs OpenFGA, SpiceDB or Cerbos, reachable from the sidecar, with its
   own availability: when it is down, protected queries fail.

## Alternatives

| | What it gives | What it leaves |
|---|---|---|
| Do nothing | No cost | Customers with sharing and teams keep hand-written policies, or move to a backend |
| Better RLS tooling (policy generation, an advisor, a "why is this row hidden") | Helps every project, no engine, no new failure mode | Relationships still live in policy SQL; inherited trees and broad subjects stay slow |
| pg_auth | Rules in one place, enforced on every surface that reads through Postgres | An engine and a sidecar to run, and an extension in the read path |

## A phased path

| Phase | Platform work | Must be true first |
|---|---|---|
| 0. Self-hosted Supabase with a design partner | None | A license; a partner with sharing or teams |
| 1. An opt-in image variant, loaded per database (`session_preload_libraries`), Unix socket only, `anon` and `authenticated` subject to it | Build and ship the variant | Upgrade scripts; loading per database tested |
| 2. The sidecar on Compute | A sidecar per project | Compute reachable privately and kept awake; the remote setup measured |
| 3. A managed engine | Run an engine per project or region | Phase 1 and 2 adoption |

Costs per phase (engineering time, a security review's scope, on-call, the engine and sidecar per
project) are not estimated yet.

Risks to weigh: availability coupling to the engine (fail closed is the safe default, but an engine
outage becomes a data-path outage); a security review of the planner hooks (unsafe Rust is in
`src/pg/` and `src/ffi.rs`, about 2,800 of the 9,000 lines outside the tests); and a new class of support
question ("my query failed because the engine was slow").

## How success would be measured

Policies replaced per design partner; protected queries' p95 against their own RLS baseline;
errors caused by the engine or the sidecar; support tickets per adopter.

## What I am asking for

1. **A go-ahead for phase 0**, time-boxed, with access to the fleet data above to size demand.
2. **A decision on ownership and a license**, so a design partner can run it.
3. **Time for a security review** of the hooks and the sidecar, before phase 1.

## Where to look

- Live demo and overview: https://filipecabaco.github.io/pg_auth/
- Benchmarks and method: https://filipecabaco.github.io/pg_auth/benchmarks/
- How it is tested: https://filipecabaco.github.io/pg_auth/testing/
- The RLS issues catalogue (draft): https://filipecabaco.github.io/pg_auth/research/rls-issues/
- The sidecar over HTTPS, and Compute: https://filipecabaco.github.io/pg_auth/proposals/external-providers/
- Source: https://github.com/filipecabaco/pg_auth
