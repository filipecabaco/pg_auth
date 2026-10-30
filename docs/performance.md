# Performance

pg_auth asks an engine outside Postgres who may see what, and still competes with row level
security written by hand and tuned: over the 489 query and subject pairs of the
[benchmarks](benchmarks.md) (35 scenarios, 2026-10-29), it takes 0.80x the time of best-practice
RLS, planning and execution together, and is faster in 201 of them, with OpenFGA deciding;
0.78x and faster in 192 with SpiceDB. How the pieces work is in
[the implementation](design.md); this page is what they cost and how to tune them.

## Where it wins and where it loses

Faster than hand-tuned RLS where a policy has to do real work (OpenFGA's run; SpiceDB's is in
[the benchmarks](benchmarks.md)):

| scenario | pg_auth vs best-practice RLS |
|---|---|
| permissions inherited down a folder tree | 0.21x |
| a policy reading another protected table | 0.22x |
| a selective subject paging an ordered table | 0.24x |
| storage paths (a key expression) | 0.34x |
| a large membership array | 0.39x |
| Supabase owner-or-role | 0.46x |

Slower where a policy is a single indexed comparison, which is RLS's best case, or where its
premise is kept:

| scenario | pg_auth vs best-practice RLS |
|---|---|
| a policy on a column with no index (the pg_auth variant keeps it unindexed) | 1.70x |
| one tenant holding half the table, paging one kind of row | 1.68x |
| reading one partition directly | 1.64x |
| an update that must also pass the read policy | 1.60x |
| a tenant column, RLS's best case | 1.38x |
| joining two protected tables | 1.23x |

On such statements, which run in under 0.1 ms, pg_auth's planning (about 0.02 ms per protected
table, warm) is a visible share of the total. Run-to-run variation per scenario is large, so a
change is judged by building both versions and running them side by side, alternately, not by
comparing with a published table ([benchmarks](benchmarks.md)).

## What costs what

- **Planning.** The planner hook asks the sidecar for the subject's answer (an id list, a plan,
  or "too many") and rewrites the policy into a filter the planner can index. Answers are kept per
  backend for half the sidecar's cache TTL, and the sidecar keeps the answers of recently active
  subjects fresh in the background, so planning rarely waits on the engine: after 1.5 s idle,
  0.47 ms (2.65 ms before the background refresh), most of it the host's cold caches.
- **A connection's first request.** Loading the configuration, preparing pg_auth's queries and
  connecting to the sidecar: `authz.warm()` does it ahead of time (first planning 0.4 ms ->
  0.15 ms). Call it as a pooler's connect query.
- **Broad subjects.** A subject who may see more objects than the engine lists has its rows
  checked in batches (one sidecar request per batch, growing to 1000 ids), and then from the
  permission index, a copy of its set kept in Postgres and updated from the engine's change feed:
  no engine calls at all in steady state.
- **Attribute rules (Cerbos).** The subject's plan is compiled into a filter on the row's
  columns; its values are read at execution, so one cached plan serves every principal whose plan
  has the same shape.
- **Writes.** Updates and deletes by a broad subject find their rows through the permission
  index; single-row writes are one check.

## Under load, and with the sidecar on another host

The benchmarks above time single statements. `bench/remote/run.exs` measures request rate
instead: concurrent clients making requests as PostgREST makes them (claims and role set in a
transaction, then one read: a page of an organisation's documents or one document), as 1,000
users drawn at random over 100 organisations and 100,000 documents. OpenFGA decides, on its
Postgres datastore; Postgres is held to 2 CPUs and 2 GB. Best-practice RLS runs on a copy of the
same tables, its policy an indexed membership lookup.

A random user rarely repeats within the half second a backend keeps an answer, so almost every
request asks the sidecar once (0.93 to 0.98 round trips per request): the case where pg_auth's
answers are reused least. 2026-10-30, 20 s per row:

| setup | clients | requests/s | vs RLS | p50 ms | p99 ms |
|---|---|---|---|---|---|
| best-practice RLS | 4 | 3070 | | 1.27 | 1.84 |
| | 16 | 5377 | | 2.93 | 4.20 |
| | 64 | 6267 | | 7.12 | 206 |
| pg_auth, Unix socket | 4 | 1804 | 0.59x | 1.92 | 7.61 |
| | 16 | 3616 | 0.67x | 3.72 | 20.9 |
| | 64 | 3687 | 0.59x | 7.22 | 210 |
| pg_auth, HTTPS, same host | 4 | 1509 | 0.49x | 2.21 | 9.44 |
| | 16 | 2688 | 0.50x | 4.25 | 33.8 |
| | 64 | 2272 | 0.36x | 26.3 | 67.2 |
| pg_auth, HTTPS, 6.2 ms more per round trip | 4 | 646 | 0.21x | 5.73 | 13.5 |
| | 16 | 2556 | 0.48x | 5.92 | 13.5 |
| | 64 | 2533 | 0.40x | 21.5 | 68.2 |

What it shows:

- **A request pays one network round trip** when its subject's answer is not kept: the ask is one
  exchange on a kept connection (a packet capture shows the request's two packets sent together
  and the answer's one), so latency added between Postgres and the sidecar is added to the
  request once. In one region that is 0.3 to 1 ms.
- **Concurrency hides the wait.** A backend waiting on the network uses no CPU, so with 16 clients
  a 6.2 ms round trip costs 5% of the throughput HTTPS on the same host has (2556 against 2688
  requests/s). With 4 clients each waits its turn, and throughput follows latency.
- **Postgres CPU sets the throughput.** Read from Postgres's cgroup, a request costs 184 µs of
  Postgres CPU under RLS and 530 µs under pg_auth over the socket (4 clients). A profile of the
  backends put most of the difference in pg_auth's own work, not the transport: parsing the
  sidecar's list answer into a generic JSON value, and for broad subjects reading the permission
  index through `security definer` SQL functions. Both were changed (list answers parsed into
  their set directly; the index read through its indexes from Rust, `src/pg/index.rs`): run
  alternately with the build before, at 16 clients, Postgres CPU per request fell from 572 and
  591 µs to 460 and 479, throughput rose from 3496 and 3381 requests/s to 4184 and 4117 (0.69x of
  RLS to about 0.85x), and p99 from about 21 ms to about 8. At 4 clients the difference was within
  the run-to-run noise. The table above is from before the change.
- **HTTPS adds CPU only under heavy concurrency.** 548 µs per request against the socket's 530
  with 4 clients; 742 against 546 with 64 clients on 2 CPUs, where each request's hand-off to the
  backend's request thread and back competes for the CPUs.
- **This is not the 0.80x above.** Those are single statements, timed with answers kept; this is
  a stream of requests by subjects not seen in the last half second. Requests by the same users
  again and again (a session's page loads) reuse answers and ask the sidecar less.

The latency comes from a relay whose packets Linux's `netem` delays. On Docker Desktop's VM it
delays by several times its setting (1 ms each way asked, 3 to 5 ms seen in a capture), so the
benchmark reports the round trip it actually added: a trip's time beyond the engine with the
relay, less without it.

## Tuning

| what | how |
|---|---|
| index the id column | as for RLS; `authz.doctor()` names the missing ones |
| warm connections | `select authz.warm()` as the pooler's connect query |
| answer freshness against engine load | the sidecar's `CACHE_TTL_MS` (default 1 s) and `REFRESH_ACTIVE_MS` (default 30 s) |
| broad subjects | `pg_auth.permission_index` (on) with the sidecar's `PG_AUTH_DATABASE_URL`; `pg_auth.fill_wait_ms`, `pg_auth.fill_wait_after` |
| cached plans shared by subjects of different breadth | `pg_auth.replan_on_breadth` (on) rebuilds them |
| statements that spend time in the engine | `pg_auth.warn_ms`, `authz.last_statement_advice()`, `authz.explain(sql)` |

## Measuring

`bench/docker/run.exs` runs the benchmark scenarios on fresh containers, and
`bench/docker/planning.exs` times one query's planning after idle gaps of different lengths, with
the sidecar calls and engine time each statement caused. `bench/remote/run.exs` measures request
rate under concurrency over the socket, over HTTPS, and over HTTPS with latency added
(`--clients 4,16,64 --latency 1`); its results go to `bench/remote/results/`. Profiling a backend or the sidecar with
samply is in [development.md](development.md).
