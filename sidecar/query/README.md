# pg-auth-sidecar

Answers the pg_auth extension's authorization questions by asking the source-of-truth engine:
OpenFGA or SpiceDB, which decide on relationships, or Cerbos, which decides on a resource's
attributes (docs/proposals/cerbos.md). It runs next to Postgres, outside it, and listens on a local Unix socket;
Postgres holds no copy of the authorization data. The protocol is engine-neutral: the extension
does not know which engine is behind the socket, and `ping` is the only answer that names it.

## Protocol

One JSON object per line; one response line per request. Ids are bare object ids.

    {"op":"list","subject":"user:anne","permission":"viewer","type":"document"}
      -> {"ids":["1","2"]}          every object the subject may access
      -> {"too_many":true}          at least LIST_LIMIT: the extension checks rows instead
      -> {"error":"..."}            the extension fails closed
    {"op":"check","subject":"user:anne","permission":"viewer","type":"document","ids":["1","9"]}
      -> {"allowed":[true,false]}
    {"op":"ping"}
      -> {"engine":"openfga"}       or "spicedb", "cerbos"
    {"op":"plan","subject":"user:anne","principal":{"roles":["user"]},"permission":"read","type":"document"}
      -> {"plan":"always"} | {"plan":"never"} | {"plan":"rows"} | {"plan":"condition","condition":{...}}

`plan` answers, for an engine that decides on attributes, which resources a subject may access
as a condition on them, in the protocol's expression language (`and`, `or`, `not`, `eq`, `ne`,
`lt`, `le`, `gt`, `ge`, `in` over `{"column":...}`, `{"id":true}` and `{"value":...}`); `rows`
when the engine's condition is outside it. Requests may carry `"principal"` (roles and
attributes) and `check` `"attrs"` (one object per id); engines that decide on relationships
ignore both.

`ids`, `too_many` and `allowed` answers also carry `engine_us`, the time the request spent waiting
on the engine (0 when answered from the cache), which `authz.stats()` sums per backend. `ids` and
`too_many` (and `plan` answers) carry `valid_ms` when the answer cache is on: how long the extension may keep the
answer, until the cached answer reaches half its TTL (`CACHE_TTL_MS`).

    {"op":"fill","subject":"user:anne","permission":"viewer","type":"document"}
      -> {"queued":true}            filled in the background (false: the index is off)

    {"op":"fill","subject":"user:anne","permission":"viewer","type":"document","wait_ms":10000}
      -> {"queued":true,"fresh":true}   answered when that fill ends (or after wait_ms): whether
                                        the set is fresh now (pg_auth.fill_wait_ms)

## Engines

The engine is Cerbos when `CERBOS_URL` is set, SpiceDB when `SPICEDB_URL` is, OpenFGA otherwise.

| | OpenFGA | SpiceDB | Cerbos |
|---|---|---|---|
| `list` | ListObjects | LookupResources, paged by its cursor | none: always `too_many` |
| `check` | BatchCheck, 50 ids a call | CheckBulkPermissions, 1000 ids a call | CheckResources with each resource's attributes, 50 a call |
| `plan` | none | none | PlanResources, translated |
| fill (permission index) | StreamedListObjects | LookupResources at the latest revision (`fully_consistent`) | none |
| change feed | ReadChanges, polled every `INDEX_POLL_MS` | Watch, streamed | none |
| API | HTTP | the HTTP gateway (`--http-enabled`) | HTTP |

**Cerbos.** A principal needs at least one role; without, the request fails. A check's answers
on attributes or a principal are not cached (they depend on more than the id); anything but an
explicit allow is a denial.

**OpenFGA.** ListObjects silently truncates at its `listObjectsMaxResults` (1000 by default), so
a full page is reported as `too_many`. It also answers with a partial list, as a success, when
its `listObjectsDeadline` (3 s by default) runs out, so an answer that took at least
`OPENFGA_LIST_DEADLINE_MS` is reported as `too_many` too: the extension then checks rows, slower
but right. Missing or failed BatchCheck results count as denied.

**SpiceDB.** LookupResources returns at most `--max-lookup-resources-limit` (1000) objects a call;
the sidecar pages past it with the cursor, every page at the first one's revision, and reports
`LIST_LIMIT` objects or more as `too_many`. Subjects may be usersets (`group:eng#member`). Only a
definite permission counts: a caveated relationship evaluated without its context is conditional,
and conditional is denied, in lists and checks alike. An id SpiceDB cannot hold (outside
`[a-zA-Z0-9/_|-=+]`) is denied without asking. Reads use `SPICEDB_CONSISTENCY`:
`minimize_latency`, SpiceDB's default, answers from a revision up to its quantization window old
(5 s by default); `fully_consistent` reads the latest. The Watch stream starts from a revision read
just before it and resumes from the last one it saw; starting from a new revision (at start, or
after SpiceDB refuses the last one, past its GC window) marks every set stale, since changes may
have been missed. An idle stream sends nothing, so TCP keepalive, not silence, detects a dead one. Watch on Postgres needs
`track_commit_timestamp = on` in SpiceDB's datastore.

## Permission index

With `PG_AUTH_DATABASE_URL` set, the sidecar keeps the extension's permission index
(`sql/index.sql`): the full sets of subjects with more objects than `LIST_LIMIT`, which the
extension then answers from Postgres instead of asking the engine per row.

- **Filling.** The extension asks (`fill`) when it meets such a subject without a fresh set. The
  sidecar marks the set filling, lists every object (an error, or for OpenFGA an answer that took
  `OPENFGA_LIST_DEADLINE_MS`, fails the fill, which marks the set stale), writes the set, and
  marks it fresh. The list reads past the engine's caches (OpenFGA `HIGHER_CONSISTENCY`, SpiceDB
  `fully_consistent`), so a fill reflects every change the feed reported before it. One fill at a
  time; repeated requests for a set being filled are dropped.
- **Freshness.** It follows the engine's change feed from the moment it starts, and any change
  marks every set stale (coarse: through inheritance a change can reach any set). A set
  invalidated while it fills never becomes fresh. The extension uses fresh sets only and asks the
  engine meanwhile.
- **Revocation delay.** A revocation reaches pg_auth when the feed reports it (parity measured
  ~0.2 s for OpenFGA, ~0.4 s for SpiceDB) and once the engine's own answers reflect it: OpenFGA's
  caches expire (10 s TTL by default), SpiceDB's `minimize_latency` reads reach the write's
  revision (5 s). Until then the engine itself may still answer with the old grant.
- **Database role.** The sidecar needs only `EXECUTE` on the writer functions:

      grant usage on schema authz to pg_auth_sidecar;
      grant execute on function authz.index_begin(text, text, text, text), authz.index_add(bigint, text[]),
          authz.index_finish(bigint), authz.index_abort(bigint), authz.index_invalidate() to pg_auth_sidecar;

## Run

    cargo build --release
    OPENFGA_URL=http://localhost:8080 OPENFGA_STORE_ID=01H... \
      PG_AUTH_SOCKET=/run/pg_auth/openfga.sock ./target/release/pg-auth-sidecar
    SPICEDB_URL=http://localhost:8443 SPICEDB_TOKEN=<preshared key> \
      PG_AUTH_SOCKET=/run/pg_auth/spicedb.sock ./target/release/pg-auth-sidecar

Or in Docker (`compare/parity.exs` runs it this way): the image runs as uid/gid 999, the official
Postgres image's `postgres` user, so a volume mounted at `/run/pg_auth` in both containers shares
the socket.

    docker build -t pg_auth-sidecar sidecar/query
    docker run -v pg_auth_sock:/run/pg_auth -e OPENFGA_URL=http://openfga:8080 -e OPENFGA_STORE_ID=01H... pg_auth-sidecar

| Variable | Default | |
|---|---|---|
| `OPENFGA_URL`, `OPENFGA_STORE_ID` | required for OpenFGA | |
| `OPENFGA_MODEL_ID` | latest model | pin a model |
| `OPENFGA_API_TOKEN` | none | bearer token |
| `OPENFGA_CONSISTENCY` | engine default | e.g. `HIGHER_CONSISTENCY` |
| `OPENFGA_LIST_DEADLINE_MS` | 3000 | OpenFGA's `listObjectsDeadline`; set both together |
| `SPICEDB_URL`, `SPICEDB_TOKEN` | required for SpiceDB | its HTTP gateway and preshared key |
| `SPICEDB_CONSISTENCY` | `minimize_latency` | or `fully_consistent` |
| `CERBOS_URL` | required for Cerbos | its HTTP API (port 3592) |
| `PG_AUTH_SOCKET` | `/tmp/pg_auth.sock` | created with mode 0660 |
| `LIST_LIMIT` | 1000 | OpenFGA: match its `listObjectsMaxResults`; SpiceDB: any, the sidecar pages |
| `CACHE_TTL_MS` | 1000 | answers are cached this long; 0 disables (staleness vs. load). A list or plan answered from the cache past half its TTL is refreshed in the background, and the extension keeps it until that half-life, so a subject asking steadily never waits on the engine |
| `REFRESH_ACTIVE_MS` | 30000 | lists and plans used within this long are refreshed before they expire even when nobody asks, so a subject back after a pause does not wait on the engine either (an idle backend's planning: 2.6 ms -> 0.5 ms with Cerbos); answers neither fresh nor active are dropped. 0 refreshes only what is asked for. Costs about two engine calls per second per active list or plan, never more than `REFRESH_CONCURRENCY` at once |
| `REFRESH_CONCURRENCY` | 8 | background refreshes in flight; when busy, the oldest answers go first |
| `ENGINE_TIMEOUT_MS` | 4000 | per engine request; keep it above `OPENFGA_LIST_DEADLINE_MS` and below the extension's `pg_auth.sidecar_timeout_ms` (5000) |
| `CHECK_CONCURRENCY` | 8 | engine check calls in flight at once for one check request (OpenFGA: a 1000-id batch is 20 calls; SpiceDB: one) |
| `PG_AUTH_DATABASE_URL` | none | enables the permission index (see above), e.g. `postgres://pg_auth_sidecar@/app?host=/run/postgresql` |
| `INDEX_POLL_MS` | 500 | how often OpenFGA's change feed is read; the retry interval for both |

Run it as a user in the Postgres OS user's group so the socket (0660) is reachable only by
Postgres. The code forbids `unsafe`.

## Postgres side

    ALTER SYSTEM SET pg_auth.sidecar_socket = '/run/pg_auth/openfga.sock';  -- superuser-only
    ALTER SYSTEM SET pg_auth.sidecar_timeout_ms = 5000;   -- the default; keep it above ENGINE_TIMEOUT_MS
    SELECT pg_reload_conf();
    SELECT * FROM authz.status();   -- 'engine' row: openfga or spicedb, or why it is unreachable

Each backend keeps one connection and caches answers for the rest of the transaction. When the
sidecar is unreachable or the engine errors, queries on protected tables fail; they never fall
back to showing rows.
