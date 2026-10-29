# Zanzibar-style backends: OpenFGA and SpiceDB (research for pg_auth)

Research date 2026-10-27. Versions seen: OpenFGA v1.22.0 (Apache-2.0), SpiceDB v1.57.0 (Apache-2.0), both via `gh api .../releases/latest` and `.../license`.
Legend: [V] = verified from a primary source cited inline; [I] = my inference; [U] = unverified.

---------------------------------------------------------------------
## 1. OpenFGA

### 1.1 Data model / schema language
- Model DSL: direct assignment `[user, team#member, user:*]`, same-object relation refs, tuple-to-userset `X from Y`, `or` / `and` / `but not`, conditions. Limits: the tupleset (relation after `from`) cannot reference another relation, and tupleset type restrictions cannot contain wildcards or usersets. [V] https://openfga.dev/docs/configuration-language
- Conditions are CEL; parameter types int/uint/double/bool/bytes/string/duration/timestamp/any/ipaddress/list/map. Tuple context (max 32KB) is merged with request context, tuple value wins. Default CEL evaluation cost cap is 100 (`maxConditionEvaluationCost`). [V] https://openfga.dev/docs/modeling/conditions ; default 100 [V] `.config-schema.json` in https://github.com/openfga/openfga
- SQL expressibility [I]:
  - Direct, userset (`group#member`), `or`: joins/UNION on the `tuple` table.
  - TTU: join `tuple` on `(object_type, object_id, relation)` to the parent object, then recurse. Recursive models (`member from parent`) become `WITH RECURSIVE`.
  - `but not` and `and`: SQL `EXCEPT` / `INTERSECT` or `NOT EXISTS`, but semantics inside recursion get tricky (stratification needed). OpenFGA's own docs warn these are costlier in ListObjects: https://openfga.dev/docs/interacting/search-with-permissions
  - Wildcards: `_user = 'user:*'` predicate. Fine.
  - Conditions: CEL in `condition_context` (protobuf BYTEA) cannot run in SQL; options are (a) reject or treat as deny/allow, (b) translate a CEL subset to SQL, (c) run `check_batch` on the conditional residue. This is the biggest gap.

### 1.2 APIs
| API | Notes | Source |
|---|---|---|
| Check | single; cache optional | https://openfga.dev/docs/interacting/relationship-queries |
| BatchCheck | `maxChecksPerBatchCheck` default 50; docs suggest parallel Check for <10 | config schema [V]; docs [V] |
| ListObjects | `listObjectsMaxResults` 1000, `listObjectsDeadline` 3s; standard + streaming variant (StreamedListObjects) | docs [V], config schema [V] |
| ListUsers | same 1000 / 3s limits; needs experimental flag in older versions (`enable-list-users`) | docs [V] |
| Expand | returns one-level tree; recurse on leaves manually | docs [V] |
| Read | raw stored tuples, no model evaluation | docs [V] |
| Read Changes (`GET /stores/{id}/changes`) | `page_size` default 50, max 100; `continuation_token`; optional `type` filter (must repeat `type` with the token); ordered; tuples not expanded; plain paginated polling, not streaming; no `start_time` documented on that page | https://openfga.dev/docs/interacting/read-tuple-changes [V] |

- Other server limits (config schema): `requestTimeout` 3s, `resolveNodeLimit` 25 (depth), `resolveNodeBreadthLimit` 10, `maxTuplesPerWrite` 100. [V] https://github.com/openfga/openfga/blob/main/.config-schema.json
- ListObjects works by walking the model graph with datastore reads, one query per hop, capped by deadline and result count. A GitHub issue reports ~1,023 datastore queries and ~1.8s for one ListObjects over ~1,000 reachable objects on Postgres, and that `max-concurrent-reads` does not bound SQL load. [V as report, not maintainer benchmark] https://github.com/openfga/openfga/issues/3326
- There is an experimental `pipeline_list_objects` flag in the config schema and many experimental `weighted_graph_check` (Check v2) entries in the CHANGELOG, which signals active reworking of the resolution engine. [V] https://github.com/openfga/openfga/blob/main/CHANGELOG.md . Maintainers log "v2 not yet implemented for ListObjects" there.

### 1.3 Consistency
- Only two modes: `MINIMIZE_LATENCY` (default) and `HIGHER_CONSISTENCY` (skip cache). Caching is off by default, in which case everything is strongly consistent. Docs explicitly state OpenFGA has no zookie equivalent today. [V] https://openfga.dev/docs/interacting/consistency
- Aligning with a PG snapshot [I]: if we read OpenFGA's Postgres tables directly, the PG snapshot is the consistency model; OpenFGA's own token does not exist.

### 1.4 Postgres datastore
- Supported engines: memory, postgres, mysql, sqlite. [V] https://openfga.dev/docs/getting-started/setup-openfga/configure-openfga
- Tuple table (migrations dir https://github.com/openfga/openfga/tree/main/assets/migrations/postgres ; 001, 003, 005, 006 read):
  ```
  tuple(store, object_type, object_id, relation, _user, user_type, ulid, inserted_at,
        condition_name, condition_context BYTEA,
        PRIMARY KEY (store, object_type, object_id, relation, _user))
  idx_tuple_partial_user / idx_tuple_partial_userset (partial on user_type)
  idx_tuple_ulid UNIQUE(ulid)
  idx_user_lookup (store, _user, relation, object_type, object_id COLLATE "C")   -- 006, replaced idx_reverse_lookup_user from 003
  changelog(store, object_type, object_id, relation, _user, operation INT, ulid, inserted_at, condition_name, condition_context,
            PRIMARY KEY (store, ulid, object_type))
  authorization_model(store, authorization_model_id, type, type_definition BYTEA [, serialized_protobuf added in 004]),
  store, assertion
  ```
  001: https://github.com/openfga/openfga/blob/main/assets/migrations/postgres/001_initialize_schema.sql ; 005 conditions: .../005_add_conditions_to_tuples.sql ; 006: .../006_add_collate_index.sql
- Object identity is `object_type` + `object_id` text; user is a single text `_user` such as `user:alice`, `group:eng#member`, `user:*`. Deletes are hard deletes (no MVCC columns); history lives in `changelog`.
- Reading directly: feasible and simple. Join `tuple` for one `store`. Subjects are free-form text and the model (type_definition protobuf BYTEA) must be decoded in Rust (prost with openfga api protos) to generate SQL. Condition context is protobuf BYTEA, so conditional tuples are opaque to SQL. [I]
- Risk: schema is internal, not a documented contract; migrations (goose) change it between releases (006 changed indexes). Version-pin and check `goose_db_version`. [I]

### 1.5 Data filtering / materialization
- No query-to-SQL compile and no Leopard-style index in OSS. The official recipe for "list with permissions" is: (1) search then BatchCheck, (2) local index built from `/changes`, flattened per model version, then final `/check` to catch revocations, (3) ListObjects then `WHERE id IN`, only if accessible set is ~1,000 or fewer. [V] https://openfga.dev/docs/interacting/search-with-permissions
- This is effectively the `watch` mirror pattern we want, but they leave the flattening to the app.
- License: Apache-2.0, CNCF project. [V] GitHub license API.

### 1.6 Rust clients
- No official Rust SDK found (https://github.com/openfga/rust-sdk returned 404). Community: `vakamo-labs/openfga-client` (typed client with model management, active Aug 2026), `openobserve/openfga-sdk`, `iyashjayesh/openfga-rust-sdk`, `sahajamoth/openfga-rust` (independent engine claim) and `tyrchen/openfga-rs`. [V existence via `gh search repos`; quality [U]]
- gRPC and HTTP are both exposed, protos at https://github.com/openfga/api, so tonic codegen is possible. [U on exact repo layout]

### 1.7 Performance
- No official latency benchmarks found. Secondary claims of 1M rps / 100B tuples are from a dev.to article citing an Auth0 post. [U]
- User reports: Check p90 about 1s at 40 concurrency on a small tuple set (issue #2906, v1.11.1), ListObjects heavy datastore fan-out (#3326). https://github.com/openfga/openfga/issues/2906

---------------------------------------------------------------------
## 2. SpiceDB (AuthZed)

### 2.1 Data model / schema
- Schema: `definition`, `relation`, `permission`; operators union `+`, intersection `&`, exclusion `-`; arrows `->` / `.any()` and the costly `.all()` intersection arrow; subject relations `group#member`; wildcards `user:*`; caveats; `use self`, `use typechecking`. [V] https://authzed.com/docs/spicedb/concepts/schema
- Caveats: typed parameters plus an expression (e.g. `user_ip.in_cidr(cidr)`), context stored on the relationship and/or supplied per request. Expression language is CEL per AuthZed's caveat docs [U in this session: the schema page did not name CEL; see https://authzed.com/docs/spicedb/concepts/caveats].
- Relationship expiration is supported (column `expiration`, migration 0021). [V] https://github.com/authzed/spicedb/blob/main/internal/datastore/postgres/migrations/zz_migration.0021_add_expiration_column.go
- Results are tri-state (HAS / NO / CONDITIONAL_PERMISSION) when caveat context is missing; LookupResources also returns possibly-conditional results [V] https://github.com/authzed/api/blob/main/authzed/api/v1/permission_service.proto (comment near `LookupResources`).
- SQL expressibility [I]: same as OpenFGA (union = UNION; arrows = join; recursion = `WITH RECURSIVE`; exclusion/intersection = EXCEPT/INTERSECT). Extras to handle: caveats (CEL), expiration (`expiration IS NULL OR expiration > now()` is trivial in SQL), `.all()` (relational division / `NOT EXISTS`), arrows ignore subject relations.
- Important nuance: the SpiceDB experimental query planner (`--experimental-query-plan`, with `lr`/`ls` modes) builds query plans/iterators over datastore queries; changelog shows active bug fixing around wildcards, recursion and caveats. It is not an exported "compile to SQL" feature. [V] https://github.com/authzed/spicedb/blob/main/CHANGELOG.md ; blog: https://authzed.com/blog/introducing-spicedb-query-planner [U, not fetched]

### 2.2 APIs (gRPC, `authzed.api.v1`)
Names from permission_service.proto [V] https://github.com/authzed/api/blob/main/authzed/api/v1/permission_service.proto :
- `CheckPermission`, `CheckBulkPermissions` (batched, one revision), `LookupResources` (server stream), `LookupSubjects` (server stream), `ExpandPermissionTree`, `ReadRelationships` (server stream), `WriteRelationships`, `DeleteRelationships`, `ImportBulkRelationships` / `ExportBulkRelationships` (streams).
- Watch is in the separate WatchService; fields: `optional_start_cursor` (ZedToken), `optional_update_kinds` (`WATCH_KIND_INCLUDE_CHECKPOINTS`), response `changes_through`, updates, `optional_transaction_metadata`. History is limited to the GC window (typically 24h, varies). [V] https://authzed.com/docs/spicedb/concepts/watch
- LookupResources paging: `optional_limit` + `optional_cursor` / `after_result_cursor`; server flag `--max-lookup-resources-limit` default 1000 [secondary: https://linen.authzed.com/t/16106977, U]. Without a cursor the server buffers before streaming (community answer, U). AuthZed's own guidance: LookupResources is best for roughly under 10,000 accessible resources. [V] https://authzed.com/docs/spicedb/modeling/protecting-a-list-endpoint
- CheckBulk batch limit: not found in sources I read (server has `--max-bulk-check` style limits [U]).

### 2.3 Consistency
- `minimize_latency`, `at_least_as_fresh(ZedToken)`, `at_exact_snapshot(ZedToken)` (can fail "Snapshot Expired" after GC window), `fully_consistent`. ZedTokens are returned by Write/Delete/Check/CheckBulk. [V] https://authzed.com/docs/spicedb/concepts/consistency
- Postgres implementation: revisions are Postgres MVCC snapshots. Code uses `pg_current_xact_id()`, `pg_current_snapshot()` (`pg_snapshot`), `xid8`, `pg_visible_in_snapshot`. [V] https://github.com/authzed/spicedb/blob/main/internal/datastore/postgres/revisions.go and watch.go
- Consequence [I]: if the extension and SpiceDB share one Postgres, a ZedToken is decodable to a pg_snapshot, so `at_exact_snapshot` and the extension's own query snapshot can be aligned with high fidelity. Decoding the token format needs the SpiceDB protobuf; format stability is not guaranteed [U].

### 2.4 Postgres datastore and direct reads
- Postgres 15+ recommended; Watch requires `track_commit_timestamp=on`; read replicas supported with replica-lag checks; GC window controls history. [V] https://authzed.com/docs/spicedb/concepts/datastores
- Tables (migrations: https://github.com/authzed/spicedb/tree/main/internal/datastore/postgres/migrations ; schema consts https://github.com/authzed/spicedb/blob/main/internal/datastore/postgres/schema/schema.go):
  ```
  relation_tuple(namespace, object_id, relation,
                 userset_namespace, userset_object_id, userset_relation,
                 caveat_name VARCHAR, caveat_context JSONB,
                 expiration TIMESTAMPTZ,
                 created_xid xid8, deleted_xid xid8 NOT NULL DEFAULT '9223372036854775807')
  relation_tuple_transaction(xid xid8, snapshot pg_snapshot, timestamp, metadata)
  namespace_config(namespace, serialized_config BYTEA, created_xid, deleted_xid)
  caveat(name, definition BYTEA, created_xid, deleted_xid)
  schema / schema_revision (chunked schema storage, migrations 0024/0025, newer)
  relationship_counter
  ```
  Initial (pre-xid) layout: zz_migration.0001_1eaeba4b8a73_initial.go ; caveats: 0009_caveat.go ; xid8: 0010_add_xid8_columns.go ; 0014 dropped bigserial ids and old transaction columns.
- Live rows are `deleted_xid = '9223372036854775807'::xid8` (soft delete, MVCC by xid). Relevant indexes: `uq_relation_tuple_living_xid`, `ix_relation_tuple_by_subject (userset_object_id, userset_namespace, userset_relation, namespace, relation)`, covering partial `ix_relation_tuple_alive_by_resource_rel_subject_covering ... WHERE deleted_xid = max`, `ix_watch_api_index (created_xid)`. [V] https://github.com/authzed/spicedb/blob/main/internal/datastore/postgres/schema/indexes.go
- Direct read from the extension is viable [I]: predicate `deleted_xid = 'max'::xid8 AND (expiration IS NULL OR expiration > now())`, filtering caveat_name IS NULL for the SQL-safe path, and schema decoded from `namespace_config.serialized_config` (protobuf `NamespaceDefinition`) or the newer `schema` table. Caveats: the table layout is internal and has been migrated 25 times; running SpiceDB's own caching/quantization means SpiceDB may legitimately answer from an older revision than the extension reads (use `fully_consistent` for parity); `pg_dump`/logical replication of this DB can desync xids (documented warning in datastores page).
- Watch can also be mirrored via the API instead of tables.

### 2.5 Data filtering / materialization / licensing
- **AuthZed Materialize**: precomputes configured permissions (Leopard-inspired), components: Offline Hydrator, Online Hydrator, Event Processor, Snapshotter, Event Log. Two features: Accelerated Queries (GA, speeds Check/CheckBulk/LookupResources/LookupSubjects transparently) and Event Streams (early access) with `LookupPermissionSets` (backfill) and `WatchPermissionSets` / `WatchPermissions` (changes) designed for syncing into an app DB or Elasticsearch. **Available on Dedicated only; commercial**, contact account team. [V] https://authzed.com/docs/authzed/concepts/authzed-materialize ; API https://buf.build/authzed/api/docs/main:authzed.api.materialize.v0
- Permission sets are a normalized form: consumer must intersect sets to get final permission (extra computation on our side). [V via search summary of buf docs, https://buf.build/authzed/api/docs/main:authzed.api.materialize.v0]
- Current limitations: caveats, wildcards and `.all` intersection arrows cannot be on the path of a Materialize-configured permission (error). Source is a docs PR (https://github.com/authzed/docs/pull/335) [V via search summary; live "Limitations" page not retrievable, URL guess 404]. Plain `&`, `-`, `->` not listed as unsupported [U, PR text truncated].
- AuthZed's list-endpoint guidance: LookupResources (<~10k results) then `WHERE id = ANY`, else CheckBulk loop, else Materialize joined locally. [V] https://authzed.com/docs/spicedb/modeling/protecting-a-list-endpoint
- **Postgres FDW** (translates SQL to SpiceDB API calls): experimental, Cloud/Dedicated only, not in OSS or Enterprise; no joins between FDW tables, no subqueries, only equality + AND pushdown; has `consistency` column accepting ZedTokens. [V] https://authzed.com/docs/authzed/guides/postgres-fdw . This is per-row-network-call-ish and does not match our goal.
- Licensing: SpiceDB OSS is Apache-2.0 [V]. Materialize and FDW are not OSS.

### 2.6 Rust clients
- `authzed` crate 0.0.1, "Rust client for authzed.com", 1,877 downloads, repo https://github.com/authzed/authzed-rs [V via crates.io API; GitHub API returned 404 for that repo, so repo existence/state [U]]. Alternative: generate tonic clients from https://github.com/authzed/api (buf.build/authzed/api), which also contains `materialize.v0` protos. [V proto repo exists]

### 2.7 Performance
- Vendor benchmark: 1M QPS, 1% writes, 5.76 ms p95 CheckPermission, up to 100B relationships on SpiceDB Dedicated + CockroachDB; mostly cache-assisted. [V] https://authzed.com/blog/google-scale-authorization . Not representative of self-hosted Postgres; community reports high p95 on small self-hosted setups. [V as reports, https://linen.authzed.com/t/28560263]
- LookupResources: v3 on a cursored iterator lib; v1.46 lower memory; experimental query planner for LR/LS (v1.50). [V] https://github.com/authzed/spicedb/blob/main/CHANGELOG.md

---------------------------------------------------------------------
## 3. Verdicts

| Capability | OpenFGA | SpiceDB (OSS) | SpiceDB + Materialize (Dedicated, commercial) |
|---|---|---|---|
| compile_filter (SQL predicate, planner-visible) | No native support. We would compile model + tuples ourselves by reading `tuple` directly. Easy for direct/userset/TTU/or; harder for and/but-not in recursion; conditions (CEL) not SQL-evaluable. | No native support (query planner is internal iterators, experimental). Compile ourselves over `relation_tuple` (xid8 soft-delete filter). Caveats are the blocker, `.all` is awkward. | Yes in effect: Event Streams give denormalized permission sets to JOIN locally. Excludes caveats/wildcards/`.all`. Not OSS. |
| check_batch | OK: BatchCheck, 50 per call default, network call. | OK: CheckBulkPermissions (single revision, one ZedToken). | Accelerated Queries speeds it. |
| watch / tuple mirror | Weak: poll `/changes` (page max 100, no stream, continuation token, per-`type`). Mirror is possible but slow to bootstrap (use `Read`). Or skip the mirror and read `tuple` directly. | Strong: streaming Watch with ZedToken cursor and checkpoints, 24h-ish history; Postgres needs `track_commit_timestamp`. Bulk export via `ExportBulkRelationships`. Or read `relation_tuple` directly. | `LookupPermissionSets` + `WatchPermissionSets`. |
| Snapshot alignment with PG | Only via shared DB and PG MVCC (no tokens). | Best: ZedToken is a pg_snapshot when datastore is Postgres. | n/a |
| Rust client | Community only | `authzed` 0.0.1 or tonic from protos | same protos |
| Maturity/risk | Engine being reworked (weighted graph v2, pipeline ListObjects). Tuple table schema simple and stable-looking. | Mature; schema layout changes often (25 migrations), proprietary Materialize. | Vendor lock, early access. |

### Recommendation
1. Treat both as two adapters over the same pattern: an in-extension compiler from the schema (decode protobuf in Rust) to SQL over locally readable tuple tables, with `check_batch` as fallback for what cannot compile (caveats/conditions, and possibly exclusion in recursive shapes).
2. For `watch`: SpiceDB Watch is the good fit; OpenFGA `/changes` is polling and weaker, so prefer sharing/reading its Postgres tables directly when co-located.
3. Biggest risks: (a) caveats/CEL conditions (both), (b) coupling to internal table layouts, (c) staleness: OpenFGA has no tokens, SpiceDB cache vs `fully_consistent`, (d) recursion depth semantics (OpenFGA `resolveNodeLimit` 25 vs SQL recursion), (e) Materialize is Dedicated-only and early access.

### Gaps not verified
- Exact CEL statement for SpiceDB caveat docs page; max CheckBulk size; Materialize live limitations page; OpenFGA `/changes` retention and whether a start_time exists; authzed-rs repo status; OpenFGA write path transactionality between `tuple` and `changelog` (assumed same transaction).
