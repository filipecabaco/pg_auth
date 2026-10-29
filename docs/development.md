# Development

How to build, test and measure pg_auth locally. The extension is a [pgrx](https://github.com/pgcentralfoundation/pgrx)
crate at the repository root; the sidecar is a separate crate in `sidecar/query`.

## Toolchain

Every tool's version is pinned in `mise.toml`, and CI uses the same file:

```sh
mise install                      # Rust (with rustfmt, clippy), cargo-pgrx, cargo-deny, cargo-machete, samply, hyperfine
cargo pgrx init --pg18 download   # once: a Postgres for pgrx to build and test against (also --pg15, --pg16, --pg17)
```

`cargo-pgrx` must match the `pgrx` version in `Cargo.toml`; bump both together. The Docker images
(`bench/docker/Dockerfile`, `sidecar/query/Dockerfile`) pin the same Rust version.

## Build and test

```sh
cargo pgrx test pg18                                   # the extension's tests, in a Postgres it starts
(cd sidecar/query && cargo test)                       # the sidecar's
cargo fmt --check && (cd sidecar/query && cargo fmt --check)
cargo clippy --all-targets --features "pg18 pg_test" -- -D warnings
(cd sidecar/query && cargo clippy --all-targets -- -D warnings)
cargo deny check && (cd sidecar/query && cargo deny check)   # advisories, licenses, sources
cargo machete                                          # unused dependencies
```

These are what `.github/workflows/rust.yml` runs. The lint policy is in each `Cargo.toml`
(`[lints]`: clippy's pedantic group, no `unwrap`/`expect` outside tests, every `unsafe` block
documented) and the dependency policy in `deny.toml`.

## Tests and coverage

The test suites, the parity checks against each engine and the coverage measurement are in
[testing.md](testing.md).

## Where `unsafe` lives

All of the extension's `unsafe` code is in `src/pg/` (and `src/ffi.rs`, the SPI wrappers). The
rest of the crate is `#![deny(unsafe_code)]`:

- `pg/mod.rs`: handles on Postgres trees (`Ptr<'a, T>`, `Node<'a>`, `List<'a, T>`). They read and
  write fields through raw places, never Rust references, and check node types by tag
  (`Node::cast`). The `fields!` macro declares a struct's accessors.
- `pg/build.rs`: new nodes, made in the callback's memory context.
- `pg/catalog.rs`: catalog lookups and expression analysis, with plain values in and owned data out.
- `pg/hooks.rs`, `pg/plan.rs`: hook entry points and planner calls. Each entry point turns what
  Postgres passes into handles and calls safe code in `src/rewrite/`.
- `pg/exec.rs`: the AuthzBatch executor. Its per-row functions run without pgrx's guard, for speed,
  and are the one place where a whole function body is an `unsafe` block.

New planner or executor logic goes in `src/rewrite/` over these handles. When it needs a field or
a Postgres function the layer lacks, add an accessor (`fields!`) or a wrapper in `src/pg/`, with a
`// SAFETY:` comment saying why it is sound.

## Profiling

Release code with symbols comes from the `profiling` cargo profile:

```sh
cargo pgrx run pg18 --profile profiling        # psql on pgrx's Postgres, with the extension built so
(cd sidecar/query && cargo build --profile profiling)
```

[samply](https://github.com/mstange/samply) records either process and opens the profile in the
Firefox Profiler:

```sh
# the backend serving a psql session: `select pg_backend_pid();` in it, then
samply record --pid <pid>            # run the statements to profile in that session; Ctrl-C to stop
samply record ./sidecar/query/target/profiling/pg-auth-sidecar   # the sidecar, as configured by its environment
```

On macOS, attaching to a running process needs `samply setup` once. On Linux it needs
`perf_event_paranoid` at 1 or lower.

For where planning time goes in a realistic setup, start a bench stack and keep it running.
`bench/docker/planning.exs` times one query's planning after idle gaps of different lengths, along
with the sidecar calls and engine time each statement caused (from `authz.stats()`):

```sh
elixir bench/docker/run.exs --scenario rls-best-case-tenant-column --variants pg_auth_cerbos --reps 1 --keep
elixir bench/docker/planning.exs --port <port from the --keep line> --query "select * from app.events where id = 1" \
  --claims '{"sub":"23c820ce-a582-728e-0298-f368a5114e3b","role":"authenticated","tenant_id":"e000342e-22c2-b525-5299-b35c4d538065"}'
elixir bench/docker/run.exs --clean
```

A rise in engine time with the gap means answers expire before they are used (see the sidecar's
`REFRESH_ACTIVE_MS`). A rise in planning with no engine time is the host itself: after an idle
period, every query plans slower until the CPU caches are warm again. For whole-command timings,
`hyperfine` compares two builds or settings:

```sh
hyperfine --warmup 3 'psql -XAtc "select count(*) from app.documents" app'
```

## The in-browser demo

The home page boots a demo in the visitor's tab: Postgres 18 with pg_auth, and OpenFGA, SpiceDB
and Cerbos each with its sidecar, on an emulated arm64 machine built by
[snowglobe](https://github.com/filipecabaco/snowglobe). `mise run demo` builds it into
`docs/demo/` (gitignored); the docs workflow builds it on an arm64 runner.

- `docker/alpine/Dockerfile` builds the extension and the sidecar for Alpine (musl), which is the
  only distro snowglobe boots: `pg_auth:alpine-arm64`.
- `docker/demo/` adds the engines (static Go binaries copied from their images, data in memory),
  a database per engine (`openfga`, `spicedb`, `cerbos`, each `pg_auth.sidecar_socket` pointing at
  its sidecar, `documents` already protected), and tmux with two psql panes: anne and bob, login
  roles whose `request.jwt.claims` are set as PostgREST would set them.
- Before the snapshot, both panes run the demo's own statements in a rolled-back transaction
  (`docker/demo/train/`), and `start.sh` does the same for each engine, so a visitor's first clicks
  find warm code, catalogs, caches and connections: snowglobe drops the page cache at the snapshot
  and warms only what was read while booting.
- `docs/javascripts/demo.js` restores the snapshot and types each of the three steps into its pane
  (alt-1 and alt-2 switch panes); the engine buttons reconnect both panes to that engine's database.

To try a change to the demo without the browser, run the image and drive tmux:

```sh
docker build --platform linux/arm64 -t pg_auth-demo docker/demo
docker run -d -t --name demo --network none pg_auth-demo
docker exec demo tmux capture-pane -p -t demo:0.1   # bob's pane
```
