# pg-auth-sidecar

Answers the pg_auth extension's authorization questions by asking the source-of-truth engine:
OpenFGA or SpiceDB, which decide on relationships, or Cerbos, which decides on a resource's
attributes (docs/proposals/cerbos.md). It runs outside Postgres and listens on a local Unix socket, or over HTTPS
for a Postgres on another host (below);
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
ignore both. `list` and `check` may carry `"context"` (values for OpenFGA conditions and SpiceDB
caveats, from `authz.configure(context => ...)`) and `"now"` (a parameter given the time the
engine is asked); lists are cached per context, checks with one are not, and the permission
index is not used for them. A ListObjects that OpenFGA refuses for a parameter the request did
not give answers `too_many`, so the rows are checked, each such one denied.

`plan` is also answered for OpenFGA and SpiceDB, for tables whose rows give conditions and caveats
values (protected with attributes): SpiceDB's is one LookupResources, as the condition `id in
<definite> or (id in <conditional> and check)`, where `{"op":"check"}` asks the engine about the
row (a `check` with its attributes, which join the request's context); OpenFGA's is `rows` (every
row checked: ListObjects takes no row's values). Either way a `check` with `attrs` gives each id
its row's values as context.

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
- **Freshness.** It follows the engine's change feed, and any change marks every set stale
  (coarse: through inheritance a change can reach any set). A set invalidated while it fills never
  becomes fresh. The extension uses fresh sets only and asks the engine meanwhile.
- **Restarts.** The feed keeps the position it has read changes to in Postgres
  (`authz_internal.feed_positions`: OpenFGA's continuation token, SpiceDB's watch cursor), once
  every change before it has marked the sets stale. A feed starting, a restart or a second sidecar,
  resumes from it: sets are kept when nothing changed meanwhile, and a change made while no
  sidecar ran marks them stale on start. With no position, or one the engine no longer serves
  (another store, past SpiceDB's GC window), every set is marked stale
  (`compare/restarts.exs`).
- **Revocation delay.** A revocation reaches pg_auth when the feed reports it (parity measured
  ~0.2 s for OpenFGA, ~0.4 s for SpiceDB) and once the engine's own answers reflect it: OpenFGA's
  caches expire (10 s TTL by default), SpiceDB's `minimize_latency` reads reach the write's
  revision (5 s). Until then the engine itself may still answer with the old grant.
- **Database role.** The sidecar needs only the functions in the `authz_sidecar` schema, which the
  extension grants to the role of the same name. Grant it to the login in `PG_AUTH_DATABASE_URL`:

      create role sidecar login password '...' in role authz_sidecar;

## Relationship tables

With `PG_AUTH_DATABASE_URL` set, the sidecar also delivers relationship tables' changes
(`authz_internal.relate`, sql/relationships.sql) to OpenFGA or SpiceDB (`src/outbox.rs`). It listens for
the trigger's `NOTIFY`, claims the oldest changes (one sidecar at a time, an advisory lock), writes
them (OpenFGA's Write with `on_duplicate`/`on_missing: ignore`, up to 100 tuples a call; SpiceDB's
WriteRelationships with TOUCH and DELETE, up to 1000), and removes them by id in the same
transaction, which also calls `authz_sidecar.answers_changed()` so every backend forgets the answers it kept.
A failure the engine may get over (unreachable, 5xx, 429) is retried every `INDEX_POLL_MS`; a
change it refuses for good (another 4xx) is set aside in the outbox with its reason, and the others
go on. Afterwards reads skip the engine's caches: OpenFGA with `HIGHER_CONSISTENCY` for 10 s,
SpiceDB at least as fresh as the write's ZedToken. A change shows within tens of milliseconds.
A relationship that ends (a note's `until`) is written with its end, which SpiceDB takes as the
relationship's expiration (`optional_expires_at`); its delete waits in the outbox until it is due,
and is delivered like any other change, on both engines.

## Engine rules

With `PG_AUTH_DATABASE_URL` set, the sidecar also pushes the rules kept in Postgres
(`authz.set_rules`, sql/rules.sql) to its engine (`src/rules.rs`). It listens for `NOTIFY
pg_auth_rules`, takes the latest version of each not yet pushed (one sidecar at a time, an
advisory lock), and writes them: OpenFGA's WriteAuthorizationModel (the DSL converted to its JSON
by `src/openfga_dsl.rs`; later requests name the new model), SpiceDB's WriteSchema, Cerbos's Admin
API (`PUT /admin/policy`, the YAML sent as JSON, with `CERBOS_ADMIN_USER` and
`CERBOS_ADMIN_PASSWORD`; Cerbos stores policies that do not compile and goes on with the ones
before, so InspectPolicies is asked after, and when they do not compile the versions before are
stored again, a new policy disabled, and the push refused). The result is recorded beside each version in the same transaction,
which calls `authz_sidecar.answers_changed()` and clears the sidecar's own caches when the engine took
them. The engine's id for the rules (OpenFGA's model id, SpiceDB's schema ZedToken) is recorded
beside them (`authz_sidecar.rules_ref`), and every sidecar asks with it from its next wake (a
`NOTIFY`, or `INDEX_POLL_MS`): OpenFGA requests pin that model, SpiceDB reads are at least as
fresh as that schema. A relationship written with a condition (`authz_internal.relate condition =>`)
carries it: OpenFGA's tuple `condition`, SpiceDB's `optional_caveat`, with the row's values. Rules the engine refuses are recorded with its reason and not retried; an unreachable engine
is retried every `INDEX_POLL_MS`.

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
| `CERBOS_ADMIN_USER`, `CERBOS_ADMIN_PASSWORD` | none | its Admin API's credentials, to push the policies kept in Postgres |
| `PG_AUTH_SOCKET` | `/tmp/pg_auth.sock` | created with mode 0660 |
| `PG_AUTH_LISTEN` | none | `host:port`: also answer over HTTPS, for an extension that reaches the sidecar by URL (below) |
| `PG_AUTH_SECRET_FILE` | required with `PG_AUTH_LISTEN` | the secrets requests are signed with, one per line, each at least 32 bytes |
| `PG_AUTH_TLS_CERT`, `PG_AUTH_TLS_KEY` | required with `PG_AUTH_LISTEN` | its certificate chain and key (PEM) |
| `PG_AUTH_TLS_TERMINATED` | off | `1` instead of the certificate, behind a platform that terminates TLS in front of it; signatures are still required |
| `PG_AUTH_MAX_CONNECTIONS` | 512 | connections open at once on `PG_AUTH_LISTEN`; one past it is closed at once |
| `LIST_LIMIT` | 1000 | OpenFGA: match its `listObjectsMaxResults`; SpiceDB: any, the sidecar pages |
| `CACHE_TTL_MS` | 1000 | answers are cached this long; 0 disables (staleness vs. load). A list or plan answered from the cache past half its TTL is refreshed in the background, and the extension keeps it until that half-life, so a subject asking steadily never waits on the engine |
| `REFRESH_ACTIVE_MS` | 30000 | lists and plans used within this long are refreshed before they expire even when nobody asks, so a subject back after a pause does not wait on the engine either (an idle backend's planning: 2.6 ms -> 0.5 ms with Cerbos); answers neither fresh nor active are dropped. 0 refreshes only what is asked for. Costs about two engine calls per second per active list or plan, never more than `REFRESH_CONCURRENCY` at once |
| `REFRESH_CONCURRENCY` | 8 | background refreshes in flight; when busy, the oldest answers go first |
| `ENGINE_TIMEOUT_MS` | 4000 | per engine request; keep it above `OPENFGA_LIST_DEADLINE_MS` and below the extension's `pg_auth.sidecar_timeout_ms` (5000) |
| `CHECK_CONCURRENCY` | 8 | engine check calls in flight at once for one check request (OpenFGA: a 1000-id batch is 20 calls; SpiceDB: one) |
| `PG_AUTH_DATABASE_URL` | none | enables the permission index, relationship tables and engine rules (see above), e.g. `postgres://sidecar@/app?host=/run/postgresql` |
| `INDEX_POLL_MS` | 500 | how often OpenFGA's change feed is read; the retry interval for both |
| `REQUEST_LOG` | off | `1` prints one line per request on stderr: what Postgres asked, the answer (a plan as the SQL filter it becomes), and whether the engine or the cache answered. For watching a setup |

Run it as a user in the Postgres OS user's group so the socket (0660) is reachable only by
Postgres. The code forbids `unsafe`.

## Over the network

`PG_AUTH_LISTEN` makes the sidecar answer the same protocol over HTTPS, one `POST /v1/ask` per
request (`src/net.rs`), for an extension that reaches it by URL instead of a socket: a sidecar
on another host, or on a platform that runs services next to the database. The socket stays
available beside it.

- **Signed.** Each request carries `pg-auth-timestamp`, `pg-auth-nonce` and `pg-auth-signature`,
  an HMAC-SHA256 over the timestamp, the nonce, the endpoint and the body with a secret both ends
  hold; each answer carries
  the sidecar's signature over it and the request's signature. A request without one, with a
  wrong one, or with a timestamp more than 30 s from the sidecar's clock is refused (401), and the
  secret never crosses the wire.
- **Replays.** A request is taken once: the sidecar remembers the signatures of the last minute,
  and the nonce makes every request's signature new; one whose timestamp is 30 s off is refused
  too. Keep the clocks of Postgres's host and the sidecar's within that (NTP): a skewed clock is
  refused, and Postgres reports it as a wrong signature. Each replica remembers its own.
- **Who reaches it.** Only Postgres should: restrict the port (a firewall, a Kubernetes
  NetworkPolicy). Its headers are checked before a body is read; a body has 10 s and 16 MiB, a TLS
  handshake 10 s, and at most `PG_AUTH_MAX_CONNECTIONS` (512) connections are open at once. There
  is no timeout on reading headers, which would also close the idle connections Postgres keeps;
  refusals are logged at most once a second.
- **Rotation.** `PG_AUTH_SECRET_FILE` holds one secret per line, any of them accepted: add the new
  one, move Postgres to it, remove the old one. The sidecar reads the file, and its certificate,
  when it starts: restart it after each change.
- **TLS.** Its own certificate, or `PG_AUTH_TLS_TERMINATED=1` behind a platform that terminates
  TLS; without either it does not start, nor without the secret. Its own connection to Postgres
  (`PG_AUTH_DATABASE_URL`) takes `sslmode` as libpq does (`src/pg.rs`): `prefer` by default, and
  `verify-full` with `sslrootcert` to check the server's certificate and name.
- **Health.** `GET /healthz` answers `ok` while the process runs; it does not check the engine or
  Postgres.
- **Several replicas.** Each keeps its own answer cache, and the one that delivers a change tells
  the others (`NOTIFY pg_auth_delivered`, with SpiceDB's ZedToken): each forgets what it cached
  and reads the engine at least as fresh, so a revocation shows through all of them at once
  (`compare/notes.exs`). Each remembers the requests it has seen on its own.
- **Try it.** `docker/remote` runs Postgres, SpiceDB and the sidecar in separate containers with
  `docker compose`, the sidecar reached only over HTTPS.
- **Cost.** About 0.2 ms a round trip more than the socket, measured on one host
  (`compare/remote.exs`: about 250 µs beyond the engine, against about 30 µs), plus the network's own round
  trip.

## Postgres side

    ALTER SYSTEM SET pg_auth.sidecar_socket = '/run/pg_auth/openfga.sock';  -- superuser-only
    ALTER SYSTEM SET pg_auth.sidecar_timeout_ms = 5000;   -- the default; keep it above ENGINE_TIMEOUT_MS
    SELECT pg_reload_conf();

    -- or a sidecar reached over the network
    ALTER SYSTEM SET pg_auth.sidecar_url = 'https://sidecar.internal:8443';
    ALTER SYSTEM SET pg_auth.sidecar_secret_file = '/etc/pg_auth/secret';  -- readable by the postgres OS user
    ALTER SYSTEM SET pg_auth.sidecar_ca_file = '/etc/pg_auth/ca.pem';      -- for a private CA; else the public roots
    SELECT pg_reload_conf();
    SELECT * FROM authz.status();   -- 'engine' row: openfga or spicedb, or why it is unreachable

Each backend keeps one connection and caches answers for the rest of the transaction. When the
sidecar is unreachable or the engine errors, queries on protected tables fail; they never fall
back to showing rows.
