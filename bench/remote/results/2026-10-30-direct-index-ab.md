Before: `9e8ad9e`. After: the same plus list answers parsed straight into their set, and the
permission index read through its indexes from Rust (`src/pg/index.rs`) instead of `security
definer` SQL functions. Run alternately (before, after, before, after), 15 s per row, RLS and the
Unix socket only. The fourth round's RLS rows at 4 clients were disturbed by something else on
the host (994 requests/s).

| round | setup | clients | requests/s | p50 ms | p95 ms | p99 ms | sidecar round trips per request | ms per trip beyond the engine | Postgres CPU µs per request | rest of the stack's CPU µs per request | errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| before | best-practice RLS | 4 | 2662 | 1.43 | 2.12 | 2.59 | 0.0 | 0.0 | 203 | 1 | 0 |
| before | best-practice RLS | 16 | 5043 | 3.13 | 3.99 | 4.51 | 0.0 | 0.0 | 293 | 1 | 0 |
| before | pg_auth, Unix socket | 4 | 1496 | 2.26 | 5.11 | 9.97 | 0.96 | 0.06 | 535 | 182 | 0 |
| before | pg_auth, Unix socket | 16 | 3496 | 3.85 | 8.24 | 20.38 | 0.97 | 0.08 | 572 | 114 | 0 |
| after | best-practice RLS | 4 | 2656 | 1.43 | 2.12 | 2.61 | 0.0 | 0.0 | 206 | 1 | 0 |
| after | best-practice RLS | 16 | 4771 | 3.29 | 4.31 | 5.0 | 0.0 | 0.0 | 328 | 1 | 0 |
| after | pg_auth, Unix socket | 4 | 1434 | 2.34 | 5.46 | 10.3 | 0.94 | 0.07 | 505 | 204 | 0 |
| after | pg_auth, Unix socket | 16 | 4184 | 3.63 | 5.47 | 7.32 | 0.95 | 0.08 | 460 | 107 | 0 |
| before | best-practice RLS | 4 | 2248 | 1.64 | 2.79 | 3.55 | 0.0 | 0.0 | 253 | 2 | 0 |
| before | best-practice RLS | 16 | 4887 | 3.21 | 4.16 | 4.79 | 0.0 | 0.0 | 312 | 1 | 0 |
| before | pg_auth, Unix socket | 4 | 1338 | 2.54 | 5.64 | 10.47 | 0.93 | 0.07 | 610 | 217 | 0 |
| before | pg_auth, Unix socket | 16 | 3381 | 3.88 | 9.44 | 22.69 | 0.96 | 0.1 | 591 | 113 | 0 |
| after | best-practice RLS | 4 | 994 | 3.06 | 7.63 | 26.84 | 0.0 | 0.0 | 503 | 6 | 0 |
| after | best-practice RLS | 16 | 3664 | 4.03 | 6.91 | 9.71 | 0.0 | 0.0 | 417 | 1 | 0 |
| after | pg_auth, Unix socket | 4 | 1219 | 2.75 | 6.33 | 11.56 | 0.94 | 0.08 | 614 | 254 | 0 |
| after | pg_auth, Unix socket | 16 | 4117 | 3.67 | 5.61 | 7.98 | 0.94 | 0.07 | 479 | 112 | 0 |
