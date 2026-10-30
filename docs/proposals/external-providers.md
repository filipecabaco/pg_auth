# Proposal: external providers, and a sidecar away from Postgres

Status: exploration (2026-10-30). Nothing implemented. It revisits a decision recorded in
[permission-index.md](permission-index.md) (2026-10-28): that the sidecar runs next to Postgres on
a Unix socket. How things work now is in [design.md](../design.md).

## Why

Two wishes that meet in one place:

- **Route decisions to systems pg_auth does not know.** The sidecar speaks to three engines,
  each written into it (OpenFGA, SpiceDB, Cerbos). A team with its own authorization service, or
  rules it would rather write as code, has no way in.
- **Run the sidecar where Postgres is hosted for you.** On a managed Postgres nothing of yours
  runs on the database host, so a sidecar on a Unix socket has nowhere to run. Supabase Compute
  could host it.

## What Supabase Compute is (2026-10-30)

From [supabase.com/compute](https://supabase.com/compute): a managed service for always-on HTTP
services and sandboxes, "in the same region and network as your primary Postgres database",
with "single-digit-millisecond queries" to it. Node and Deno; "any Dockerfile in Private Alpha".
Per-workload egress firewalls and scoped secrets. Idle workloads suspend and resume in under a
second (scale to zero). Private alpha, behind a waitlist. The page does not say whether a service
takes raw TCP, or whether it can be reached privately rather than from the internet.

**What it does not solve:** the extension. pg_auth's planner hook needs the library in
`shared_preload_libraries`, and hosted Supabase does not load third-party libraries there.
Compute moves the sidecar, not the extension: on hosted Supabase this goes nowhere until pg_auth
ships in Supabase's Postgres image. Self-hosted Postgres can use all of it.

## Part 1: an HTTP provider in the sidecar

A fourth engine, `http`, that sends the sidecar's questions to any URL implementing a small
contract, so a team writes its rules as a service (TypeScript on Compute, say) or puts a thin
adapter in front of another system. The sidecar keeps what makes pg_auth fast and correct: its
answer cache, batching, the permission index and the outbox. The extension does not change.

The contract is the sidecar's own protocol (`sidecar/query/src/protocol.rs`), one POST per
question, JSON in and out:

```text
{"op":"describe"}                                      -> {"ops":["list","check","plan","write","changes"]}
{"op":"list","subject":"user:anne","permission":"viewer","type":"document","limit":1000}
                                                       -> {"ids":["1","2"]} | {"too_many":true}
{"op":"check","subject":"user:anne","permission":"viewer","type":"document","ids":["1","9"]}
                                                       -> {"allowed":[true,false]}
{"op":"plan", ...}                                     -> as the protocol's plan (attribute rules)
{"op":"write","changes":[{"write":true,"object":"org:1","relation":"member","subject":"user:bob"}]}
                                                       -> {"refused":[{"index":0,"reason":"..."}]}
{"op":"changes","cursor":"..."}                        -> {"changed":true,"cursor":"..."}
```

- `list` and `check` are required; `check` carries `attrs`, `context` and `principal` as the
  protocol does. `plan` lets attribute rules compile into filters, as Cerbos's do.
- `write` makes relationship tables work (the outbox delivers to it); without it a table cannot
  declare `rows of`.
- `changes` is the change feed. Without it the permission index stays off for the provider, since
  nothing would tell a set it went stale, and broad subjects are checked in batches.
- Credentials: a bearer token from the sidecar's environment. Hosted OpenFGA (Okta FGA) also needs
  OAuth client credentials, which the sidecar does not do yet; the same token exchange would serve
  both.

Cost: one new engine module and a contract test. It can be built and tested now, with a provider
written for the test, and is useful without Compute.

## Part 2: the extension reaching a sidecar over the network

What a sidecar away from Postgres needs:

- **Framing.** Compute exposes HTTP services, so each request becomes an HTTPS POST over a
  connection kept per backend, from a synchronous client inside Postgres (`ureq` with rustls; no
  async runtime in a backend). If raw TCP is offered, the JSON lines go over TLS as they are.
- **Authentication.** On a Unix socket the file's permissions keep everyone out. A network sidecar
  answers "what may subject X see" for any X: it needs a token the extension sends
  (`pg_auth.sidecar_token`, superuser-only like `pg_auth.sidecar_socket`) or mutual TLS, and must
  not be reachable from the internet.
- **Latency.** A Unix socket round trip is tens of microseconds; the same region's network about
  0.3 to 1 ms, and a TLS handshake each time a backend connects. Answers kept across transactions
  (1 s) and the permission index absorb most of it; a narrow subject's first statement and
  per-row batches pay it. Many of pg_auth's wins over row level security are sub-millisecond, so
  expectations come from measuring (below), not from these numbers.
- **The sidecar's own Postgres connection** needs TLS (it connects with `NoTls` today).
- **Scale to zero is a correctness question.** The sidecar does work no request asks for: it
  delivers relationship changes when Postgres notifies it, ends grants when they are due, pushes
  rules, and follows the engine's change feed. Suspended, the feed misses changes while the
  permission index still holds sets marked fresh. So either the sidecar never suspends, or the
  index is trusted only while a feed has followed up to now: a feed that replays from the oldest
  point a fresh set depends on (the restart work of 2026-10-30) makes a resumed sidecar catch up,
  and the extension would wait for that before using a set.

### Measuring first

Before any Compute work: run the benchmarks with latency added between the extension and the
sidecar (a TCP relay inserting 0.5, 1 and 2 ms, the extension speaking TCP), and publish the
ratios against best-practice row level security per scenario, as `docs/benchmarks.md` does. That
says which workloads a remote sidecar suits.

## Questions for Supabase

- Can a Compute service be reached only from the project's network, and over TCP as well as
  HTTP?
- Can a service opt out of suspending, or be woken by Postgres `NOTIFY`?
- Could pg_auth ship in Supabase's Postgres image, loadable in `shared_preload_libraries`?

## Phases

1. The HTTP provider (Part 1), with its contract and an end-to-end check like the engines'.
2. The latency measurement, with a TCP transport behind a setting.
3. If the numbers hold and Supabase answers yes: HTTPS framing, the token, TLS to Postgres, and a
   deployment guide for Compute.
