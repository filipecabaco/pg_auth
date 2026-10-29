# Policy-engine backends for compile_filter / check_batch (research, 2026-10-27)

Legend: [V] = verified from primary source fetched; [S] = from search snippet / secondary; [U] = unverified/inferred.

## Summary table

| Engine | Model | Residual / partial eval | Residual -> Postgres SQL | In-process Rust | Entity data | Verdict |
|---|---|---|---|---|---|---|
| Cedar (cedar-policy) | RBAC+ABAC+ReBAC-ish (entity hierarchy `in`, attrs, tags) | Yes, experimental `tpe` (type-aware) feature; older `partial-eval` superseded | No official translator; DIY from residual AST (`pst`) or experimental cedar-sql | Yes, Apache-2.0, v4.14, ~10.6M downloads | Passed per request (Entities / PartialEntities) | Best compile_filter candidate; also strong check_batch |
| AVP (Verified Permissions) | Cedar, managed | No partial eval API found [U] | n/a | No (network) | Passed per request or in policy store | check_batch only; not fit for hot path |
| OPA/Rego | ABAC (RBAC/ReBAC by data) | Yes, mature (Compile API), plus first-class data-filter API | Yes, official: OPA emits postgres/mysql/sqlite/sqlserver/UCAST | OPA itself: no (Go; wasm only without PE). regorus: yes but no OPA-style PE | `input` per request or `data` in OPA | Best-supported translator, but must run as sidecar/Go; Rust path weak |
| regorus | Rego | Experimental, different from OPA PE | Not yet | Yes, MIT, v0.13 | input/data in engine | check_batch OK; compile_filter immature |
| Oso (OSS) | Polar, RBAC/ReBAC/ABAC | `partial` unification, data-filtering adapters (legacy) | ORM adapters, not raw SQL | Rust polar-core exists, but repo archived 2026-10-22 | App-supplied | Do not build on |
| Oso Cloud | Polar, ReBAC-centric | Yes: Local Authorization `list_local` emits SQL | Yes, Postgres/MySQL (service generates) | No, hosted closed service | Facts in Oso Cloud + your DB (SQL mapped via YAML) | Network hop per query; vendor risk |
| Casbin (casbin-rs) | ACL/RBAC/ABAC via model files | No | No | Yes, Apache-2.0, v2.20 | Policies in memory; adapters load | check_batch only (enforce); no filtering |

## 1. Cedar

**Policy model.** Policies are permit/forbid over principal/action/resource with `when`/`unless`; ABAC on entity attributes and context, RBAC/ReBAC via entity parent hierarchy (`principal in Group::"x"`) and tags. Entities are explicitly supplied to the authorizer. [V] https://docs.rs/cedar-policy/latest/cedar_policy/ , RFC https://github.com/cedar-policy/rfcs/blob/main/text/0095-type-aware-partial-evaluation.md

**Partial evaluation.**
- Two features, both `experimental` in Cargo.toml of cedar-policy: `partial-eval` (old, untyped, `Authorizer::is_authorized_partial`, returns Allow/Deny/Residual) and `tpe` (type-aware, `PolicySet::tpe(request, entities, schema) -> TpeResponse`). Docs say prefer `tpe`. [V] https://github.com/cedar-policy/cedar (cedar-policy/Cargo.toml features), https://docs.rs/cedar-policy/latest/cedar_policy/
- Old partial-eval is outside semver guarantees, no formal correctness proof, produces redundant `true && unknown(resource)["isPublic"]` terms. [V] https://cedarland.blog/usage/partial-evaluation/content.html ; semver note [S] cedar-py release notes.
- TPE: partial request with known principal/action/context and unknown resource *id* but known resource *type*; schema is required. RFC example: "list documents Alice can view" yields residual policies `resource.isPublic` and `resource.owner == User::"Alice"`; RFC explicitly says these can be translated to SQL. TPE also gives `is_authorized_batched` (loads entities via an `EntityLoader`, iterative) and `ActionQueryRequest`/`PrincipalQueryRequest` (find actions/principals). [V] RFC + docs.rs. RFC header still says "Accepted: TBD, Stabilized: TBD" (as fetched). [V]
- Active development: changelog through 4.14 shows TPE changes in nearly every release (residual inspection, PST conversion of residuals for structured walking, template-linked policies, perf on large policy sets, `has/in/==` reduction when schema allows). [V] https://github.com/cedar-policy/cedar/blob/main/cedar-policy/CHANGELOG.md . Open issue to allow partially-known attributes in TPE (#1735) [S] https://github.com/cedar-policy/cedar/issues/1735
- Interaction with entity hierarchy: unknown-resource conditions like `resource in Folder::"x"` or `principal in resource.owners` are the hard part: they need a closure/ancestor lookup in SQL (recursive CTE or materialised closure table). [U] inferred; cedar-sql README models hierarchy as a single `cedar_entity_hierarchy(descendant, ancestor)` table. [V] https://github.com/luxas/cedar-sql

**Residual -> SQL.** No official translator. Residual is a Cedar expression tree over: `==`, `!=`, `&&`, `||`, `!`, `<`/`<=` on Long, `in`, `has`, `.attr` access, `contains`/`containsAll`/`containsAny` on sets, `like`, tags (`hasTag/getTag`), extension types (`ip`, `decimal`, `datetime`). Mapping most to SQL is easy (attr -> column, `==` -> `=`, set contains -> `= ANY`/array ops); hard: `in` (hierarchy), tags (side table), records (jsonb), extension types, `like` wildcards -> LIKE escaping, error semantics (Cedar errors make a policy not apply; SQL NULL semantics differ - must wrap in `COALESCE(..., false)` and preserve forbid-overrides-permit: result = OR(permit residuals) AND NOT OR(forbid residuals)). [U] operator list is my inference from the Cedar language; the mapping notes are analysis, not sourced. Existing translators: blog's example app does it by hand [V] cedarland blog; `luxas/cedar-sql` is an experimental Cedar-to-SQL compiler (Postgres/Turso) with explicit "MUST NOT be used in production" warning [V]; awzy-security issue #281 sketches residual->AST for SQL [S].

**Embedding.** Pure Rust, `cedar-policy` v4.14.0 on crates.io (~10.6M downloads), Apache-2.0, last push 2026-10-26, 1.8k stars. [V] gh api + crates.io. Supports wasm. Good fit for pgrx (no network; no tokio needed). Caveat: `tpe` is behind an experimental feature flag, so API churn is expected.

**Data source.** Entities must be passed per request (JSON or built). For check_batch with local data, the extension would materialise entity attributes from tables (SPI) into `Entities` - cost scales with entity count and hierarchy depth. For compile_filter, only principal entity (and its ancestors) needs loading; resource data stays in Postgres - this is the ideal split. TPE's `PartialEntities` can also leave resource attrs unknown.

**Performance.** Cedar paper claims microsecond-scale evaluation, formal verification of core [S] https://arxiv.org/pdf/2403.04651 . Changelog notes `PolicySet::tpe` faster on large policy sets [V]. No published TPE benchmarks found [U].

**Verdict.** Best fit overall for compile_filter. Risks: experimental TPE API; you own the residual->SQL compiler (and its soundness; use property tests: for random rows, SQL result == `is_authorized` result); hierarchy/tags translation; schema required.

### Amazon Verified Permissions
- Managed Cedar. APIs: IsAuthorized, BatchIsAuthorized (max 30 requests, entities up to 100 principals/100 resources; principal or resource must be identical across batch). [V] https://docs.aws.amazon.com/verifiedpermissions/latest/apireference/API_BatchIsAuthorized.html
- I found no partial-evaluation / residual / list-resources API in AVP. [U] (searched; absence not proven)
- Not embeddable, network hop. Useful only as a check_batch backend with the 30-per-call cap; poor for filters. Policy store (Cedar) could be synced down and evaluated locally with cedar-policy instead.

## 2. OPA / Rego

**Model.** Rego is a Datalog-like policy language; ABAC natively, RBAC/ReBAC by encoding relations in `data`/`input`. Data comes from `input` (per request) or `data` (loaded/pushed bundles). [S] https://www.openpolicyagent.org/docs

**Partial evaluation / data filtering.**
- Classic Compile API `POST /v1/compile` (query, input, unknowns, options.disableInlining, nondeterministicBuiltins) returns residual queries (AST). [V] https://www.openpolicyagent.org/docs/rest-api
- New first-class data-filtering API in open-source OPA: `POST /v1/compile/{path}` (e.g. `/v1/compile/filters/include`), unknowns declared in policy METADATA (`# compile: unknowns: [input.fruits]`), target chosen by `Accept` header: `application/vnd.opa.sql.postgresql+json`, `...mysql+json`, `...sqlserver+json`, `...sqlite+json`, `application/vnd.opa.ucast.{all,minimal,linq,prisma}+json`, or `application/vnd.opa.multitarget+json`. Response `{"result":{"query":"WHERE ..."}}`; empty query = unconditional allow; missing `query` = unconditional deny. [V] https://www.openpolicyagent.org/docs/rest-api , https://www.openpolicyagent.org/docs/filtering/tutorial-sql-filtering
- Code lives in OPA repo (`v1/server/compile_handler.go`, `v1/compile`), Apache-2.0, pushed daily. [V] gh api open-policy-agent/opa. Styra's Enterprise OPA (features originated there) was moved to open-policy-agent/eopa, Apache-2.0, last push 2026-06-26. [V] gh api; context [S] Apple hired Styra's core maintainers Aug 2025, CNCF retains OPA, Styra DAS sunsetting https://www.heise.de/en/news/Apple-acquires-developer-of-the-Open-Policy-Agent-10574287.html , https://cloudnativenow.com/features/apple-buys-styra-brains-opa-remains-open/ . Which exact distribution (OPA vs EOPA) shipped each SQL target is [U].

**Supported fragment (important limits).** [V] https://www.openpolicyagent.org/docs/filtering/fragment
- OK with unknowns: `== != < > <= >=` with exactly one side unknown (both-unknown only for SQL targets); existence check `field = _` (-> IS NOT NULL); `startswith/endswith/contains` with unknown on LHS only; `k in ...` membership (not `k, v in`); rules/functions without `default`/`every` over unknowns; `not` on simple comparisons/builtin calls.
- NOT OK with unknowns: `regexp.match` and any other builtin; `every`; `default` rules; bare unknown as expression; `not` of unknown or of a rule ref; unknown on RHS of startswith etc.
- With only known values, all of Rego is allowed. So: policy author must write "fragment-compatible" Rego; relationship traversal over unknown tables (joins) is not expressible as OPA unknowns of the form `input.table.col` - data filter model is single-table columns. Joins/recursion to other tables must be pre-resolved against known data, i.e. you push the principal's group/role closure as known data. [partly U: inferred from unknown naming `input.<table>.<col>`]

**Embedding in Rust.**
- OPA is Go; cannot link into pgrx. Rego -> WASM compile exists but WASM output has no partial evaluation (PE is a compile-time/server feature of OPA) [U]. Practical options: sidecar OPA over localhost (network hop but only per *query plan*, cacheable per (subject-shape, action, table)), or regorus.
- regorus (Microsoft): Rust Rego interpreter, MIT (LICENSE file; GitHub shows NOASSERTION), v0.13.0, ~4.6M downloads, no_std capable, ~OPA v1.2.0 conformance minus some builtins (no crypto by design). [V] https://github.com/microsoft/regorus , crates.io.
- regorus partial evaluation: issue #604 (open) - maintainers say their PE differs from OPA's ("quite different from generating SQL or residual policies as OPA does"); an experimental mode returning structured `residual_queries` was added (Apr 2026 comments) and a Rego-subset -> KQL/SQL converter prototype was mentioned; status unmerged/unclear. [V] https://github.com/microsoft/regorus/issues/604

**Performance.** regorus README claims faster than OPA on benchmark policies (hyperfine comparison) [S, vendor]. No PE cost numbers in OPA docs (page doesn't discuss) [V]. PE results are cacheable per principal-context.

**Verdict.** Strongest existing Rego->SQL pipeline, but only via OPA server; Rust-native path (regorus) lacks mature PE. Good for check_batch via regorus (data from tables loaded as `data`/`input`). Risks: fragment restrictions (no regex, no every, no joins), operational sidecar, org churn around Styra. Option: use OPA compile at policy-deploy time? Not possible since unknowns plus known context vary per request; but caching plan per subject-role set mitigates.

## 3. Oso

**OSS library.** Polar language, Rust `polar-core`. GitHub README: "legacy Oso open source library. It's no longer maintained, and this repository is archived and read-only" (archived 2026-10-22), Apache-2.0. [V] https://github.com/osohq/oso ; deprecation notice from Dec 2023 [S] https://www.osohq.com/docs/oss/project/changelogs/2023-12-18.html . Do not build on it.

**Oso Cloud.** Polar-based, ReBAC/RBAC/ABAC with facts; hosted service. List filtering: `list` (returns IDs - the known non-scalable path) and `list_local` / `authorize_local` / `evaluate_local` / `actions_local` (return SQL fragments/queries). Config YAML maps fact signatures to SELECT queries over your tables, plus optional `sql_types` so generated SQL can use indexes. Postgres and MySQL; Mongo beta. [V] https://www.osohq.com/docs/develop/facts/local-authorization , https://www.osohq.com/docs/develop/enforce/list-filtering
- Limits: no rules granting access to all resources based on DB data; no inequality comparison between Oso Cloud data and DB data; recursive rules can't mix centralized and local facts. [V] local-authorization doc
- Data: Oso "does not read your data", only schema; centralized facts stored in Oso Cloud are merged into generated SQL. [V]
- Network: the SQL is generated by the service, so each call is a network hop (the docs I fetched don't show a local engine) [U on whether a local/fallback engine exists].
- Not embeddable; closed. Corporate status: Cerbos blog (competitor) claims Oso Cloud contracts moved to a holding company and the team went to Temporal in Oct 2026; Oso says not shutting down. [S, unverified] https://www.cerbos.dev/blog/oso-alternatives (claim surfaced via search summary; I did not open it).
- Pricing/licensing: proprietary SaaS.

**Verdict.** Best-designed UX for the exact "local filter" concept (and a design reference: facts-to-SQL mapping config), but not usable as in-process backend; vendor risk. Could be a remote backend with `compile_filter` cached per (subject, action, table).

## 4. Casbin

- Models: PERM metamodel (`request_definition`, `policy_definition`, `role_definition`, `matchers`, `effect`) -> ACL, RBAC (with domains), ABAC via expressions on request attrs; policies are rows loaded from adapters. [S] https://casbin.apache.org
- casbin-rs: Apache-2.0, crate `casbin` v2.20.0, ~3.4M downloads, pushed 2026-10-20. [V] gh api/crates.io. Embeddable in-process. Postgres adapter: casbin-rs/sqlx-adapter (async, sqlx; MySQL/Postgres/SQLite), last push 2026-10-13; source listing shows only adapter/actions/models/error (no filtered-adapter module) [V file list; U on full absence]. Issue #79 "Implement FilteredAdapter" for diesel/sqlx adapters is old and open [S] https://github.com/casbin/casbin-rs/issues/79 . pgrx note: sqlx is async/networked; you'd write your own SPI-backed adapter.
- Data filtering: "Filtered policy loading" loads a *subset of policies* (e.g., per tenant), not a filter over application rows; `SavePolicy` disabled when filtered. [V] https://casbin.apache.org/docs/policy-subset-loading/ . Casbin's own guidance for data permissions: implicit permission APIs or `BatchEnforce`. [S] https://v3.casbin.org/docs/data-permissions . Casbin issues #634/#668 (data filtering) remain unresolved questions [S].
- No residual / partial evaluation. Matchers are expression strings (Rust: evalexpr/rhai) - arbitrary, not analyzable to SQL.
- Perf: policies in memory; enforce per (sub,obj,act) linear in policy count unless using RBAC indexes; known memory issue for millions of policies (#79) [S].
- Verdict: check_batch (loop enforce, or get_implicit_permissions_for_user then compute) only. For RBAC-shaped models you could derive compile_filter yourself: implicit permissions of a user -> set of (object-pattern, action) -> predicate; but that is a custom compiler, not a Casbin feature.

## Cross-cutting design implications

1. Real compile_filter candidates ranked: Cedar TPE (embeddable, residual is clean AST, you write translator) > OPA compile API (translator exists, but external process and restrictive fragment) > Oso Cloud (works, hosted) >> regorus PE (experimental) > Casbin (none).
2. For every engine the principal-side data (roles, groups, org memberships) is "known" and resource-side attributes are "unknown" columns. Joins to other tables (e.g. `resource.project_id` -> project membership) are the crux: Cedar `in` and Oso relations need EXISTS/closure-table SQL; OPA fragment does not do joins on unknowns. The extension must define a canonical mapping (entity type -> table, attr -> column, parent relation -> FK or closure table) like cedar-sql and Oso's YAML.
3. Soundness hazards for generated SQL: NULL tri-state vs Cedar's error-means-not-applicable; forbid precedence; extension types; use `COALESCE`/`IS TRUE` and differential tests against the in-process engine (check_batch as oracle).
4. Cache plans per (subject policy-relevant attributes, action, table); residual depends only on known inputs.
5. Items I could not verify: AVP partial eval absence; whether OPA wasm supports PE; Oso Cloud local-engine/fallback; Cedar TPE benchmarks; Oso Cloud ownership change; casbin-rs filtered adapter absence; exact OPA release version that shipped `/v1/compile/{path}` SQL targets.
