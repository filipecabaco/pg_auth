# Proposal: external providers, and a sidecar away from Postgres

Status: phase 1 implemented (2026-10-30): the sidecar reached over HTTPS with signed requests
(`pg_auth.sidecar_url`, `src/remote.rs`; `PG_AUTH_LISTEN`, `sidecar/query/src/net.rs`), checked
by `compare/remote.exs`. The HTTP provider and Compute are not started. It revisits a decision recorded in
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

## Part 2: a sidecar next to Postgres, or an external service

The goal (2026-10-30): the extension reaches its sidecar either next to Postgres, on a Unix socket
as now, or as an external service over the network, chosen by a setting
(`pg_auth.sidecar_socket`, or `pg_auth.sidecar_url` with its secret). One protocol either way.
What answers at the URL decides what keeps working:

| behind the URL | what works |
|---|---|
| pg_auth's sidecar, run elsewhere (Compute, a VM) | everything: its answer cache, the permission index, delivering relationship rows, rules pushed from Postgres |
| any service implementing the extension's protocol | decisions on rows (`list`, `check`, `plan`). The index, the outbox and pushing rules are the sidecar's work through its own Postgres connection: such a service answers `fill` with `{"queued":false}` and leaves relationship tables and `set_rules` out |

What an external sidecar needs:

- **Framing.** Compute exposes HTTP services, so each request becomes an HTTPS POST over a
  connection kept per backend, from a synchronous client inside Postgres (`ureq` with rustls; no
  async runtime in a backend). If raw TCP is offered, the JSON lines go over TLS as they are.
- **Authentication.** On a Unix socket the file's permissions keep everyone out. A network sidecar
  answers "what may subject X see" for any X: it must know the caller is Postgres, and must not be
  reachable from the internet. Decided below: signed requests.
- **Latency.** A Unix socket round trip is tens of microseconds; the same region's network about
  0.3 to 1 ms, and a TLS handshake each time a backend connects. Answers kept across transactions
  (1 s) and the permission index absorb most of it; a narrow subject's first statement and
  per-row batches pay it. Many of pg_auth's wins over row level security are sub-millisecond, so
  expectations come from measuring (below), not from these numbers.
- **The sidecar's own Postgres connection** takes TLS as libpq does, from the URL's `sslmode`
  (`verify-full` checks the certificate and the name; done 2026-10-30).
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

### Transport and authentication (decided 2026-10-30)

HTTPS only, and both ends prove themselves: Postgres must know it talks to its sidecar (a fake one
could answer "allow everything"), and the sidecar must know the caller is Postgres (anyone else
could ask what any user may see). Requests are signed rather than carrying a token, so a proxy
that sees or logs them never holds the secret, and it works behind a platform that terminates TLS
in front of the service, which Compute likely does and which strips client certificates.

**Settings**, superuser-only like `pg_auth.sidecar_socket`:

| setting | |
|---|---|
| `pg_auth.sidecar_url` | `https://...`: when set, used instead of the socket. `http://` is refused |
| `pg_auth.sidecar_secret_file` | a file holding the shared secret (at least 32 bytes), read by the backend: the secret is never in `pg_settings`. Required with the URL |
| `pg_auth.sidecar_ca_file` | PEM roots to trust instead of the public ones (Mozilla's), for a private CA |

**On the wire.** `POST <url>/v1/ask`, the body the request's JSON, as one line on the socket is;
the response body the answer's JSON.

```text
pg-auth-timestamp: 1793350000
pg-auth-nonce:     <new for every request>
pg-auth-signature: v1=<hex HMAC-SHA256(secret, "v1\n<timestamp>\n<nonce>\nPOST\n/v1/ask\n<body>")>
                              response: v1=<hex HMAC-SHA256(secret, "v1 response\n<request signature>\n<status>\n<body>")>
```

The response's signature binds the answer to the question it answers. The endpoint is signed by
its name, `/v1/ask`, whatever path a proxy forwards it under.

**Measured** (`compare/remote.exs`, one host, no network between): a round trip beyond the
engine's own time is about 250 µs over HTTPS, its connection kept and its wait cancellable,
against about 30 µs over the socket.
A deployment adds its network's round trip, 0.3 to 1 ms in one region.

**The sidecar.** `PG_AUTH_LISTEN=host:port` opens the network listener (the socket can stay
beside it). `PG_AUTH_SECRET_FILE` is required: one secret per line, any of them accepted, so a
secret is rotated by adding the new one, moving Postgres to it, then removing the old one; the
response is signed with the secret that verified the request. TLS from `PG_AUTH_TLS_CERT` and
`PG_AUTH_TLS_KEY`; without them it refuses to start unless `PG_AUTH_TLS_TERMINATED=1` says a
platform in front of it terminates TLS (signatures are still required). It answers 401 to a
missing or wrong signature, a timestamp more than 30 s from its clock, or a signature it has seen
in the last minute (the nonce makes each new), checking the headers before reading a body; 413 to
a body over 16 MiB; 404 to anything but `POST /v1/ask` and `GET /healthz` (which says only "ok").
It compares signatures in constant time, limits open connections, and gives a TLS handshake or a
body 10 s.

**The extension** verifies the certificate (the public roots, Mozilla's, or the CA file), signs each
request, keeps a connection per backend, and verifies the response's signature. Any failure fails
the statement, as a sidecar that does not answer does now. A fill wait is short waits (250 ms) one
after another, so a cancel or `statement_timeout` still applies.

| attack | what stops it |
|---|---|
| someone who reaches the sidecar asks what a user may see | no secret, no signature: 401 |
| a captured request sent again | refused: its signature was seen (each request's nonce makes it new), or its timestamp is 30 s old |
| a request changed on its way | the body is signed |
| a fake sidecar (DNS, a misrouted service) answers "allow everything" | the certificate does not verify; behind TLS termination, it cannot sign the answer without the secret |
| a proxy or platform logs the traffic | the logs hold signatures, never the secret; they do hold the questions and answers, in the clear behind a platform that terminates TLS |
| a role points pg_auth at its own sidecar, or reads the secret | the settings are superuser-only; the secret is in a file the postgres OS user reads |

## Cerbos

- **Nothing to resume.** Cerbos keeps no relationships, so there is no permission index or change
  feed: its decisions are plans compiled into filters on the row's columns, and a restart loses
  only the sidecar's plans kept for a second.
- **The best fit for a remote sidecar.** One plan per subject and action, which Postgres then
  applies to every row itself: the network is paid per statement, not per row, except for a
  condition outside the filter language, checked per row.
- **Its storage must survive restarts.** The sidecar pushes a version of the rules once, when it
  is stored. A Cerbos on in-memory storage (the demo's) that restarts has no policies and denies
  everything until a new version is stored: on Compute, where idle services suspend, it needs its
  `postgres` storage driver (the project's own database can hold it) or a file that persists. The
  sidecar could also push the version in force again when Cerbos does not have it.
- **Cerbos Hub** manages the policies itself and gives its PDPs a read-only store: the rules stay
  in Hub, so `authz.set_rules` and its notes do not apply, and tables are set up with
  `authz_internal.protect`.

## Questions for Supabase

- Can a Compute service be reached only from the project's network, and over TCP as well as
  HTTP?
- Can a service opt out of suspending, or be woken by Postgres `NOTIFY`?
- Could pg_auth ship in Supabase's Postgres image, loadable in `shared_preload_libraries`?

## Phases

1. The transport (Part 2): HTTPS and signed requests in the extension and the sidecar, checked
   end to end against the engines, and the latency it adds measured against the socket.
2. The HTTP provider (Part 1), with its contract and an end-to-end check like the engines'.
3. If Supabase answers yes: TLS from the sidecar to Postgres, and a deployment guide for Compute.
