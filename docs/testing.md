# Testing

pg_auth decides which rows a query returns, so its tests check two things: that every
enforcement path gives the same rows the engine would, and that it fails closed when anything
goes wrong. Four layers do that, and a coverage measurement shows what they reach.

## The suites

| suite | where | what it checks | runs |
|---|---|---|---|
| extension tests | `src/lib.rs` and `src/*`, `cargo pgrx test pg18` | 88 tests inside a real Postgres against a fake sidecar: each enforcement path (listed ids, one-id pages, batched checks, the permission index, hierarchies, compiled attribute rules) against the per-row check; every id type (`int4`, `int8`, `uuid`, `text`); writes; cached plans reused across subjects; a sidecar that answers late, hangs up, garbles or errors; `authz.why`, `authz.doctor`, `authz.lint` | Postgres 15, 16, 17 and 18 |
| sidecar tests | `sidecar/query`, `cargo test` | OpenFGA, SpiceDB and Cerbos mocked over HTTP ([wiremock](https://github.com/LukeMathWalker/wiremock-rs)): the exact requests sent, batching, paging, change feeds, error mapping; the request loop over a socket, with its caches and background refresh | every push |
| parity | `compare/parity.exs` | a random model, tuples and data loaded into a real OpenFGA or SpiceDB, then every subject's rows through each enforcement path compared with what the engine lists, on images with Postgres assertions on; also writes, revocation, read-only transactions, hierarchies and the permission index | before merging planner or executor changes |
| attribute parity | `compare/attributes.exs` | random rows and principals against a real Cerbos: the compiled filter, a cached plan reused by every principal, and the per-row policy, each compared with Cerbos's CheckResources | the same |
| relationship tables | `compare/relationships.exs` | an app with no backend granting and revoking by writing rows through RLS, on a real OpenFGA and SpiceDB: members, shares, teams, a non-admin refused, the engine down (changes wait, then arrive; an invalid one is set aside), and how long each change takes to show | the same |
| complex setups | `compare/complex.exs` | the engines' published shapes as a backend-less app sets them up, checked through what each user sees and may write: GitHub (teams inside teams, access to a user or a team, the org owner, a ban with `but not`, push to update) and Drive (nested groups, a folder tree, a public `user:*` link) on OpenFGA and SpiceDB; expense approval on Cerbos (derived roles, an amount limit, a tenant's scoped deny) | the same |
| engine rules | `compare/rules.exs` | the rules kept in Postgres (`authz.set_rules`) pushed by the sidecar to a real OpenFGA (its DSL), SpiceDB (a schema) and Cerbos (YAML through its Admin API, policies in a database): rows show as each version says, the same text is not a new version, rules the engine refuses are recorded with its reason, failed by `authz.doctor()`, while it keeps the version before (Cerbos's put back), and a setup naming what the rules lack fails at commit | the same |
| grants that end | `compare/expiry.exs` | relationship tables with an end (`expires_column`) on a real OpenFGA and SpiceDB: a share shows until its end and goes after it (timed from the end), an end moved later or earlier, a renewal, ends held in the outbox not counted as waiting, and SpiceDB ending it itself with the sidecar paused | the same |
| conditions and caveats | `compare/conditions.exs` | OpenFGA conditions and SpiceDB caveats on a real engine, each step through a sidecar that lists and one that checks rows: the row's values (allowed regions, a deadline) with the request's (a token's claim, the time asked), a request missing a value not let in, the row's new values followed, a deadline passing (timed); a protected table's own values (a report's level against a clearance), with how many rows each engine was asked about; rules from Postgres pushed by one sidecar and followed by the other | the same |
| security scenarios | `bench/docker/scenarios` | the correctness scenarios of the [RLS issues catalogue](research/rls-issues.md) (leaking views, recursion, partitions, leaky functions, pooled identity, bypass paths), each with pg_auth's verdict in its README | with the benchmarks |

The extension's tests run on pgrx's Postgres, built without assertions: they do not catch a
planner node freed while still in use. The parity checks run the bench image, whose Postgres has
assertions on.

```sh
cargo pgrx test pg18                                      # or pg15, pg16, pg17
(cd sidecar/query && cargo test)
elixir compare/parity.exs --seed 1                        # OpenFGA
elixir compare/parity.exs --engine spicedb --seed 1
elixir compare/attributes.exs --seed 7 --rows 2000 --principals 200
elixir compare/relationships.exs                          # OpenFGA, then SpiceDB
elixir compare/complex.exs                                # OpenFGA, SpiceDB, then Cerbos
elixir compare/rules.exs                                  # the same
elixir compare/expiry.exs                                 # OpenFGA, then SpiceDB
elixir compare/conditions.exs                             # the same
```


## Coverage

On 2026-10-29, the unit suites covered 93.5% of the lines of both crates, and with the
integration checks 96.8%. Writing tests for what was left found two bugs: a Cerbos condition on the
resource id compiled to a filter that matched nothing, and the check that `authz.allowed_*` only read
protected columns matched partitions only (both failed closed).

`scripts/coverage.exs` measures line coverage per file for both crates, three ways:

- **unit**: the extension's tests (run inside Postgres backends, which inherit
  `cargo llvm-cov`'s instrumented environment and write their profiles on exit) and the sidecar's
  unit tests (which mock OpenFGA, SpiceDB and Cerbos over HTTP);
- **integration**: the parity checks in `compare/` on images built with `-C instrument-coverage`
  (tagged `:coverage`, so no other run uses them), every sidecar and Postgres process writing its
  profile into `target/coverage/profiles` when it stops;
- **combined**: a line covered by either.

```sh
elixir scripts/coverage.exs                  # all of it, ~30 minutes; target/coverage/summary.md
elixir scripts/coverage.exs --only unit      # the unit tests again, a few minutes
elixir scripts/coverage.exs --only report    # the report from the last runs' lcov files
```

For a quick look at one crate, `cargo llvm-cov` works directly (in `sidecar/query`), and for the
extension:

```sh
eval "$(cargo llvm-cov show-env --sh)"; cargo llvm-cov clean --workspace
cargo pgrx test pg18 && cargo llvm-cov report --summary-only    # --html for a browsable report
```

## Continuous integration

`.github/workflows/rust.yml` runs on every push and pull request, with the toolchain
`mise.toml` pins: formatting, clippy with warnings as errors, the sidecar's tests, the extension's
tests on each of Postgres 15, 16, 17 and 18 (PGDG packages), the unit coverage summary, and
cargo-deny and cargo-machete for the dependencies. The parity checks need Docker and an engine per
run, and are run locally (`scripts/coverage.exs` runs them too).
