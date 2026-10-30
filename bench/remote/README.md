# Request rate, and the sidecar on another host

`run.exs` measures what pg_auth costs under concurrency, and what a sidecar away from Postgres
adds. Concurrent clients make requests as PostgREST makes them (claims and role set in a
transaction, then one read) as 1,000 users drawn at random over 100 organisations and 100,000
documents, against best-practice RLS on a copy of the same tables:

- **best-practice RLS**: an indexed membership lookup, the claim read once per statement
- **pg_auth, Unix socket**: the sidecar beside Postgres
- **pg_auth, HTTPS**: a second sidecar listening on TLS with signed requests, same host
- **pg_auth, HTTPS, added latency**: the same sidecar behind a relay whose packets `netem` delays

OpenFGA decides, on its Postgres datastore with its caches on; Postgres is held to 2 CPUs and
2 GB; every run starts fresh containers and removes them after.

```sh
elixir bench/remote/run.exs                                # builds the images first
elixir bench/remote/run.exs --no-build --seconds 20 --clients 4,16,64 --latency 1
```

Each row reports requests/s, p50/p95/p99, the sidecar round trips per request and each trip's
time beyond the engine's (both from `authz.stats()` over 200 requests on one connection). `netem`
delays by what the kernel's timers make of its setting, several times it on Docker Desktop, so
the header gives the round trip actually added: a trip's time with the relay, less without it.

Results are written to `results/<date>-<commit>.md`; what they mean is in
[docs/performance.md](../../docs/performance.md#under-load-and-with-the-sidecar-on-another-host).
