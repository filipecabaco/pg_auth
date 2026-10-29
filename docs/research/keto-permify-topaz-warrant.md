# ReBAC backends B: Ory Keto, Permify, Topaz, Warrant + Postgres-native prior art

Research date: 2026-10-27. "UNVERIFIED" marks claims not confirmed from a primary source.
Repo health values come from `gh api repos/<owner>/<repo>` (pushed_at, license, latest release).

## Summary verdict table

| Backend | compile_filter (SQL/data-filter) | check_batch | watch/tuple sync | Direct PG table read feasible | License | Health |
|---|---|---|---|---|---|---|
| Ory Keto | No | Yes (`BatchCheck`) | No Watch RPC; poll-less => none | Possible but awkward (UUID-mapped strings, no snapshot cols) | Apache-2.0 | Active (v26.2.0, 2026-03-20) |
| Permify | No SQL generation; `LookupEntity` returns IDs (the non-scalable path) | Yes (`BulkCheck`, max 100) | Yes, gRPC `Watch` stream (PG + commit ts) | Best of the four: MVCC xid8 columns, plain tables | AGPL-3.0 | Active but acquired by FusionAuth (Nov 2025) |
| Topaz | No (embedded BoltDB, not SQL) | Yes (`Checks`, parallel) | No watch; has `Exporter` (full export) | No (bbolt) | Apache-2.0 | Active; company (Aserto) shut down Apr 2025, community maintainers |
| Warrant | No | Weak (`/v2/authorize` with anyOf/allOf, not a true batch) | No | Possible (`warrant` table, soft delete, text policies) | Apache-2.0 | Effectively dormant; hosted sunset |

Bottom line: none of the four supports SQL compile/data-filter natively. All would reduce to (c) tuple mirroring + our own in-Postgres evaluation. Permify is the only one with a real Watch stream and a PG schema designed around snapshot reads; Keto and Warrant have clean simple tuple tables but no changefeed; Topaz is not Postgres-backed at all.

---

## 1. Ory Keto

**Model/schema.** Ory Permission Language (OPL), a TypeScript subset: classes implementing `Namespace`, `related` (relations, always arrays, subject->object direction), `permits` (boolean functions) with `||`, `&&`, `!`, `traverse`, and `SubjectSet<N,R>`. No attribute/condition (ABAC) support is mentioned. Names <= 1024 bytes. Source: https://www.ory.com/docs/keto/reference/ory-permission-language
- SQL mapping: direct tuples, subject sets, `traverse` (tuple-to-userset), union map to joins / recursive CTE. `!` and `&&` map to NOT EXISTS / intersection, but negation inside recursion needs care. Mapping is my analysis, not a Keto feature.

**APIs.** gRPC `ory.keto.relation_tuples.v1alpha2`: `Check`, `BatchCheck` (CheckService), `Expand`, `ListRelationTuples` (read, paginated), write (transact), namespaces, version. Protos: https://github.com/ory/keto/tree/master/proto/ory/keto/relation_tuples/v1alpha2
- `CheckRequest`: `latest`, `snaptoken`, `max_depth`. `BatchCheckRequest`: `repeated tuples`, `latest`, `snaptoken`, `max_depth`; results carry per-item `error`. https://github.com/ory/keto/blob/master/proto/ory/keto/relation_tuples/v1alpha2/check_service.proto
- No Watch/changefeed RPC (proto dir has check, expand, namespaces, read, write, version only). No ListObjects / ListSubjects RPC: the docs describe "list objects a user can access" only as a use of `ListRelationTuples` with partial tuples (reverse lookup is one hop, not a computed permission). https://www.ory.com/docs/keto/concepts/api-overview
- Depth: request `max_depth` clamped to global max-depth. Strict mode returns FAILED_PRECONDITION/422 for indeterminate checks. (same docs page)

**Consistency.** `snaptoken` field in proto is documented "not implemented yet and has no effect" (comment in check_service.proto above). Effectively read-your-writes against the primary DB only.

**Datastore.** Postgres, MySQL, SQLite, CockroachDB. Final tuple table `keto_relation_tuples` (renamed from `_uuid` in https://github.com/ory/keto/blob/master/internal/persistence/sql/migrations/sql/20220513200600000001_drop-old-non-uuid-table.up.sql):
```
shard_id UUID, nid UUID (network/tenant), namespace VARCHAR(200), object UUID, relation VARCHAR(64),
subject_id UUID NULL, subject_set_namespace VARCHAR(200) NULL, subject_set_object UUID NULL,
subject_set_relation VARCHAR(64) NULL, commit_time TIMESTAMP; PK (shard_id, nid)
```
Source: https://github.com/ory/keto/blob/master/internal/persistence/sql/migrations/sql/20220513200300000000_create-intermediary-uuid-table.postgres.up.sql
- Strings (object ids, subject ids) are mapped to UUIDs via `keto_uuid_mappings(id UUID PK, string_representation TEXT)` (https://github.com/ory/keto/blob/master/internal/persistence/sql/migrations/sql/20220513200400000000_create-uuid-mapping-table.up.sql). So joining to app tables requires a join to the mapping table (or UUIDv5 recomputation: UNVERIFIED how ids are derived).
- Indexes: forward (nid, namespace, object, relation, subject...) and reverse subject partial indexes (see migration above, plus `20220512151000000000_indices.up.sql`). Hard deletes (no tombstones, `DELETE FROM` in relationtuples.go) => cannot derive a changefeed from the table; `commit_time` exists but deletes are invisible.
- Reading directly from our extension: technically yes (same DB), schema is stable-looking, but internal, Ory does not promise it as an API.

**Data filtering/local eval.** None. Check evaluated in Go (`internal/persistence/sql/traverser.go` issues SQL per traversal step; whether it uses recursive CTEs: UNVERIFIED).

**License/health.** Apache-2.0; 5.4k stars; latest release v26.2.0 on 2026-03-20; last push 2026-09-04 (gh api). Versioning moved from 0.x to calendar versions (CHANGELOG: 0.14.0 2025-03 -> 25.4.0 2025-11 -> 26.2.0 2026-03).

**Rust.** Crate `ory-keto-client` 26.2.0 (OpenAPI/REST client, updated 2026-03-24) per crates.io. gRPC: use tonic with protos from the repo (no official tonic crate found).

**Verdict.** compile_filter: no backend support; could implement ourselves over `keto_relation_tuples`, but only by building our own OPL->SQL compiler. check_batch: yes, native `BatchCheck` (per-row network still, so fallback only). watch: not available; mirroring would require polling `ListRelationTuples` (no change cursor, deletes undetectable) or wrapping Keto's write path / PG logical decoding on `keto_relation_tuples`. Risks: unimplemented snaptoken, no ListObjects, UUID mapping, hard deletes, OPL has no conditions.

---

## 2. Permify

**Model/schema.** Schema language: `entity`, `relation name @type` (with `@team#member` locking), `permission`/`action`, `attribute` (boolean, string, integer, double and arrays), `rule` (CEL-based boolean function), operators `and`/`or`/`not`, dot traversal (`parent.admin`). Names <= 64 chars. https://fusionauth.io/permify-docs/getting-started/modeling (formerly docs.permify.co, redirects).
- SQL mapping: relations/union/dot traversal map to joins/recursive CTE; `attribute` is a table (`attributes`, jsonb value) so simple comparisons map to joins; CEL `rule` bodies would need a CEL->SQL translator (hard in general, feasible for a subset).

**APIs** (proto services in https://github.com/Permify/permify/blob/master/proto/base/v1/service.proto):
- Permission: `Check`, `BulkCheck` (max 100 per call; comment in proto), `Expand`, `LookupEntity` (page_size 1-100, `continuous_token`), `LookupEntityStream`, `LookupSubject`, `SubjectPermission`. https://fusionauth.io/permify-docs/api-reference/permission/lookup-entity
- `Watch.Watch(WatchRequest{snap_token}) -> stream WatchResponse{changes{snap_token, data_changes[]}}`; `DataChange{operation CREATE/DELETE, tuple | attribute}`. gRPC only; needs Postgres with `track_commit_timestamp=on` and `service.watch.enabled: true`. https://fusionauth.io/permify-docs/api-reference/watch/watch-changes
- Watch ops: server-side polling loop per stream, tied to one pod; resume by passing last `snap_token`; `database.watch_buffer_size` default 100; fan-in recommended. https://fusionauth.io/permify-docs/operations/watch
- Data service: `Write`, `WriteRelationships`, `ReadRelationships`, `ReadAttributes`, `Delete`, `DeleteRelationships`, `RunBundle`. Tenancy and Schema services too.
- Max-depth/limits: bulk 100, concurrency_limit and bulk_limit in config (per GitHub issue config, UNVERIFIED default values beyond 100).

**Consistency.** Snap tokens (encode a point in time / transaction id) returned by Write; pass in Check (and Lookup via `metadata.snap_token`) for at-least-as-fresh reads. Docs do not mention xid8 but the schema uses xid8 transaction ids (below). https://fusionauth.io/permify-docs/operations/snap-tokens

**Datastore.** In-memory or Postgres (Watch requires Postgres). Migrations: https://github.com/Permify/permify/tree/master/internal/storage/postgres/migrations
```
relation_tuples(id serial PK, tenant_id varchar, entity_type, entity_id, relation, subject_type, subject_id, subject_relation (NOT NULL, '' for none),
                created_tx_id xid8 DEFAULT pg_current_xact_id(), expired_tx_id xid8 DEFAULT MAX 9223372036854775807)
attributes(id, tenant_id, entity_type, entity_id, attribute, value jsonb, created_tx_id, expired_tx_id)
schema_definitions(tenant_id, name, serialized_definition bytea, version char(20))
transactions(id xid8, snapshot pg_snapshot, timestamp, tenant_id)
tenants(id, name, created_at)
```
Sources: 20230117143902_initial.sql, 20230117145140_multi_tenant.sql, 20230729145034_attributes.sql, 20241018182912_add_watch_indexes.sql (indexes on (tenant_id, created_tx_id, expired_tx_id)), 20250913120000_fix_xid_wraparound.sql (active rows now expired_tx_id = MAX, previously 0). Subject index: `idx_tuples_subject (subject_type, subject_id, subject_relation, entity_type, relation)`.
- Reading directly: yes and well-suited. MVCC-style versioning means "current tuples" = `expired_tx_id = 9223372036854775807::xid8` (post-2025-09 migration; older DBs used 0 - version-sensitive). Our extension could read tuples natively and even do snapshot-consistent reads. Strings are plain varchar, so joins to app tables are easy. Caveat: `serialized_definition` is protobuf bytea, so the schema must be fetched via the API (or decoded by us) to compile it.
- Note: table is append/expire; deletes are visible (expired rows), enabling trigger/poll mirroring independent of the Watch API.

**Data filtering.** "Lookup Entity (Data Filtering)" returns entity IDs via graph traversal in Go (`internal/engines/entity_filter.go`, `lookup.go`); the docs describe it as finding relations for subject/permission then querying them, with optional `scope` map. No SQL predicate generation. Issue "Powerful get all SQL filtering" (#27) is closed with no feature evidence: https://github.com/Permify/permify/issues/27 (outcome UNVERIFIED). This is exactly the ListObjects-then-`ANY(...)` pattern we want to avoid.

**License/health.** LICENSE file is AGPL-3.0 (gh api repos/Permify/permify/license). When it changed from Apache-2.0 is UNVERIFIED. Linking/embedding Permify code into a Postgres extension would be copyleft; reading its tables and using its wire API does not require linking. Latest release v1.7.5 (2026-10-09); last push 2026-10-23. Acquired by FusionAuth, announced 2025-11-20; docs moved to fusionauth.io; repo says "Permify is now part of FusionAuth"; says it stays open source. https://permify.co/post/fusionauth-acquires-permify/

**Rust.** No official Rust SDK. Community `3dt-digital/permify-rs` (MIT, last push 2024-12-19, 0 stars); not on crates.io. Protos in repo (`proto/base/v1`) so tonic/prost generation is straightforward; buf.build module availability UNVERIFIED.

**Verdict.** compile_filter: no native support, but best table design to build our own (tuples + attributes in plain PG with xid8 versions). check_batch: `BulkCheck` max 100/call. watch: strong, native `Watch` stream with snap_token resume (PG only, per-pod polling), or bypass it by reading `relation_tuples` created/expired tx ids directly. Risks: AGPL, FusionAuth acquisition direction, CEL rules in ABAC, bytea schema, xid8 expired sentinel changed in 2025.

---

## 3. Topaz (Aserto)

**Model/schema.** Directory "inspired by Zanzibar": objects (type + id + JSON properties, usable for ABAC), relations (object, relation, subject, optional subject_relation), permissions defined in YAML manifest (`model: version 3`, `types.<t>.relations`, `.permissions` with `|` union; arrows/`->` and exclusion/intersection exist in newer manifests: UNVERIFIED). https://www.topaz.sh/docs/directory . Policy decisions are made by OPA/Rego with `ds.check`, `ds.graph` built-ins; ABAC via Rego over object properties. README: https://github.com/aserto-dev/topaz
- SQL mapping: the manifest is the Zanzibar subset (union, traversal). Rego policies are arbitrary logic and not SQL-mappable (OPA partial eval to SQL exists generally; Topaz support for it UNVERIFIED).

**APIs.** gRPC `aserto.directory.reader.v3.Reader`: `GetObject`, `GetObjectMany`, `GetObjects`, `GetRelation`, `GetRelations`, `Check`, `Checks` (batch, executed with a worker pool sized to GOMAXPROCS), `CheckPermission`, `CheckRelation` (latter two being deprecated in favor of `Check`), `GetGraph`. Writer (`SetObject`, `SetRelation`, deletes), Importer, Exporter (full-state stream), Model (manifest get/set) services exist. Sources: https://github.com/aserto-dev/go-directory/tree/main/aserto/directory ; method names from `reader_grpc.pb.go`; `Checks` impl at https://github.com/aserto-dev/topaz/blob/main/internal/eds/pkg/ds/checks.go ; deprecation note at https://www.topaz.sh/docs/command-line-interface/topaz-cli/test
- No Watch/changefeed RPC found in the Reader/Writer/Importer/Exporter/Model packages. `Exporter` can dump all objects/relations (full resync, not incremental). `datasync` package in repo syncs from Aserto's now-closed control plane (relevance: UNVERIFIED).

**Consistency.** Single-node embedded store with bbolt transactions; no snapshot tokens found. Not Zanzibar-style distributed consistency.

**Datastore.** Embedded bbolt (`go.etcd.io/bbolt` in go.mod; `internal/eds/pkg/bdb`). No Postgres support found (code search for "postgres" in repo returned nothing). Remote directory option exists via gRPC to another Topaz/Aserto directory. Therefore our extension cannot read its tables; only the Exporter gRPC stream or Reader `GetRelations` would feed a mirror.

**Data filtering/local eval.** None in directory. OPA decision engine runs inside Topaz (OPA v1.21.4 in go.mod). Local evaluation inside Postgres not possible with Topaz itself.

**License/health.** Apache-2.0; v0.33.25 released 2026-10-26; active push. But Aserto the company shut down its SaaS control plane by 2025-05-31 and wound down; a set of maintainers continues the OSS project and may offer commercial support (post dated 2025-04-27). https://www.aserto.com/blog/the-final-chapter-for-aserto . Single-vendor copyright (relicensing risk) noted by third parties: UNVERIFIED.

**Rust.** No official crate found (crates.io `aserto`, `topaz` not relevant). Protos are on buf.build/aserto-dev/directory and in go-directory, so tonic is feasible.

**Verdict.** compile_filter: no. check_batch: yes, `Checks`. watch: none; only full export (`Exporter`) or polling `GetRelations`. Weakest fit because storage is bbolt (we'd mirror via gRPC, no PG tables), its power is in Rego/OPA which does not compile to SQL, and its company is gone. Also verdict as a tuple source: acceptable for import only.

---

## 4. Warrant (WorkOS)

**Status.** Acquired by WorkOS 2024-04-23 (WorkOS announcement tweet https://x.com/WorkOS/status/1782774170146791776; summaries https://guptadeepak.com/ciam-compass/changelog/workos-acquires-warrant-2024/). Third-party profile claims the hosted Warrant service (`api.warrant.dev`) and Warrant-based WorkOS FGA were deprecated and sunset on 2025-11-15, with a re-architected FGA coming: https://github.com/api-evangelist/warrant-dev (secondary source, partially UNVERIFIED; I could not confirm with a WorkOS primary notice). WorkOS docs currently describe FGA as active and "continuing to expand", offering access check, reverse resource lookup and reverse subject lookup: https://workos.com/docs/fga (that is the new/rearchitected product, closed source; its API compatibility with Warrant is UNVERIFIED).
- OSS repo https://github.com/warrant-dev/warrant: Apache-2.0, latest release v1.15.1 (2024-06-10), last push 2025-12-05 (README change only; previous commits Jan 2025 dependabot). Treat as unmaintained.

**Model.** Object types with JSON `definition` (relations + inheritance rules `inheritIf` anyOf/allOf), warrants = tuples, roles/permissions/features/tenants as built-in object types. ABAC: per-warrant `policy` text (expression, e.g. `geo == "us"`), evaluated in Go from request `context` (`pkg/authz/warrant/policy.go`; migration 000004 converted prior context rows into policies). https://github.com/warrant-dev/warrant/blob/main/migrations/datastore/postgres/000004_add_policy_column_to_warrant_table.up.sql
- SQL mapping: relations/inheritance map to joins; policy expressions depend on a request context so they cannot be statically compiled to SQL (would need session-GUC context + translator).

**APIs.** REST only: `/v2/authorize` and `/v2/check` (`CheckManySpec{op: anyOf|allOf, warrants[], context, debug}`; this is a multi-warrant single decision, not a per-item batch), `GET /v1/query` (Warrant Query Language, `select ... of type ... where subject is relation`, parsed in `pkg/authz/query/parser.go`) for list objects, Objects/Warrants/ObjectTypes CRUD. `MaxWarrants = 10000` fetched per check step (`pkg/authz/check/service.go`). No Watch/changefeed. Sources: repo files above; https://github.com/warrant-dev/warrant/tree/main/pkg/authz

**Consistency.** "wookie" version token: table `wookie(id, ver bigserial, created_at)`, returned as a warrant-token on writes (`pkg/wookie`). Granularity/semantics: UNVERIFIED beyond that.

**Datastore.** MySQL, Postgres, SQLite. Postgres tuple table (https://github.com/warrant-dev/warrant/blob/main/migrations/datastore/postgres/000001_init.up.sql, `000004_add_policy_column_to_warrant_table.up.sql`):
```
warrant(id bigserial PK, object_type, object_id, relation, subject_type, subject_id, subject_relation DEFAULT '',
        policy text NOT NULL DEFAULT '', policy_hash varchar(64), created_at, updated_at, deleted_at timestamp NULL)
UNIQUE (object_type, object_id, relation, subject_type, subject_id, subject_relation, policy_hash)
object(id, object_type, object_id, ... deleted_at); object_type(type_id, definition jsonb, ...)
```
- Reading directly is feasible and simple (varchar ids, soft delete via `deleted_at`, `updated_at` trigger gives a cheap polling cursor per table, with caveat that unique constraint makes re-creation after soft delete behavior UNVERIFIED). Roles/permissions/tenants are separate tables plus object rows.

**Data filtering/local eval.** None; check pipeline runs in Go with per-step DB fetches.

**Rust.** None official (SDKs: Go, Node, JS, Java, Python, Ruby, PHP, CLI per https://github.com/orgs/warrant-dev/repositories). crates.io `warrant` is an unrelated crate. REST is simple enough for `reqwest`.

**Verdict.** Not recommended as a supported backend. compile_filter: no; check_batch: partial/awkward; watch: none (could poll `updated_at`). Only use: one-time or polling import of its `warrant` table for existing users.

---

## 5. Prior art: Zanzibar/ReBAC inside Postgres

GitHub repo searches (`gh api search/repositories`) on 2026-10-27:

| Project | What | Notes | URL |
|---|---|---|---|
| pgfga (isaacharrisholt) | OpenFGA-model subset in pure PL/pgSQL; `check_permission` functions; relationships exposed through an `authz_relationship` view over app tables | MIT, 79 stars, push 2026-04; "used in production at getrover.com"; no list_objects/compile in README excerpt | https://github.com/isaacharrisholt/pgfga |
| fga4postgres (emfga) | OpenFGA-compatible engine in PL/pgSQL (PG18+, uuid ids); check, batch_check, expand, list_objects, list_users; CEL conditions via pure-SQL cel4postgres; "compiled relations": per-relation SQL functions like `document__viewer__objects('user',$1)` usable in `id IN (SELECT ...)` | Apache-2.0, very new (push 2026-10-20), pre-release, conformance-tested against openfga v1.19.0; ships plain SQL and pg_tle. Closest to our compile_filter idea | https://github.com/emfga/fga4postgres |
| demesne (foir-io) | Spec language compiled to Postgres RLS policies plus SECURITY DEFINER functions, Go/TS capability map | Apache-2.0, 7 stars, active 2026-10; topology/tenancy oriented; emits RESTRICTIVE policies for `require` | https://github.com/foir-io/demesne |
| rowstile | Policy file compiled to SQL views, trigger-maintained inheritance tables, RLS policies, `authz.can()`, `authz.perms_of()`; no extension needed (PG 16-18) | Apache-2.0, beta 0.1.0, one maintainer, notes limits with Supabase Data API roles | https://github.com/rowstile/rowstile |
| pgzan | pgrx extension to call a Zanzibar check from RLS | 2023-06, single commit, hardcoded ACL, "do not use in production"; no license | https://github.com/brahmlower/pgzan |
| zanzigo | Go library, Postgres storage, benchmarks functions vs queries | benchmark shows PG functions faster than client queries for nested checks (127us vs 190us per search snippet; not independently verified) | https://github.com/trevex/zanzigo |
| postkit | Postgres-native infra incl. identity/authz SQL | Apache-2.0, small | https://github.com/varunchopra/postkit |

Findings relevant to design: (1) the pure-SQL prior art converges on materialized or recursive-CTE evaluation inside PL/pgSQL and per-relation SQL functions that the planner can inline (fga4postgres compiled relations) - matches our compile_filter; (2) RLS-compilers (demesne, rowstile) show permissive/restrictive policy generation is viable; (3) no existing pgrx/hook-based implementation beyond the toy pgzan was found (search `pgrx authorization` returned no repos). General recursive-CTE depth/performance caveats (practical depth 3-4 hops; a 335k-node tree took 47s in one blog's case) are from secondary blog posts: https://evokoa.com/blog/postgres-as-a-graph-database/ and https://dev.to/ineron/your-postgresql-already-has-a-graph-engine-you-just-have-to-build-it-2ng7 (not verified).

## Recommended integration approach (from findings)

- Treat all four as tuple *sources*, and compile from our own local tuple tables.
- Permify: best direct read path (xid8 versioned tables) and a real Watch stream; license AGPL means do not link its code.
- Keto: polling impossible for deletes without logical decoding on `keto_relation_tuples`; UUID mapping needs resolving.
- Topaz/Warrant: import-only adapters; low priority.
- Our own Rust client work: tonic from protos for Permify/Keto/Topaz; reqwest for Warrant/REST.
