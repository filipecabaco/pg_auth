# How large authz services get fast, and what maps into Postgres (pg_auth)

Scope constraint (from user): enforcement lives in the DB for every client, no app layer. Pain points to judge against:
(P1) function calls in policies, (P2) joins/subplans in policies, (P3) unindexed selects / seq scans, (P4) partitioning.
Legend: VERIFIED = read in primary source this session. SNIPPET = only seen via search-result summary. UNVERIFIED = not found.

---------------------------------------------------------------------------
## 1. Google Zanzibar (USENIX ATC'19) -- VERIFIED from PDF text
URL: https://research.google/pubs/zanzibar-googles-consistent-global-authorization-system/ (PDF: https://storage.googleapis.com/gweb-research2023-media/pubtools/5068.pdf)

Scale: >2 trillion tuples, ~100 TB, 10M+ client QPS, p95 < 10 ms, 99.999% avail (sections 1, 4). Check peak ~4.2M QPS; Check Safe latency p50/95/99/99.9 ~ 3/11/20/93 ms (sec 4.1-4.2).

### 1.1 Leopard (sec 3.2.4) -- the key idea for us
- What: a separate index of *named sets* as (T, s, e) tuples: T=set type, s=set id (64-bit), e=element id (64-bit). Query = union/intersection/exclusion of named sets, result ordered by element id, limited.
- Group membership uses two set types: GROUP2GROUP(ancestor) -> all transitive descendant groups; MEMBER2GROUP(user) -> groups the user is a *direct* member of. Check "U in G" = MEMBER2GROUP(U) INTERSECT GROUP2GROUP(G) != empty.
  So nested groups are flattened only on the group->group side (transitive closure); users stay un-flattened. This bounds the write amplification: user add = 1 row, group edge add = (#ancestors x #descendants) rows.
- Storage: ordered integer lists (skip lists); intersection cost O(min(|A|,|B|)) seeks; sharded by element id; mostly RAM, optionally SSD.
- Three parts: (a) serving system, (b) offline periodic builder from a tuple snapshot (recursively expands usersets rewrite rules), shards swapped atomically, (c) online incremental layer: (T,s,e,t,d) with timestamp t and delete marker d, merged over offline index at query time for updates with t <= query timestamp. Incremental indexer consumes the Watch changelog stream and must keep group-to-group state to denormalize an update.
- Cost: "a single Zanzibar tuple addition or deletion may yield potentially tens of thousands of discrete Leopard tuple events". Steady-state incremental writes: ~500/s median, ~1.5K/s p99 (sec 4.4).
- Read perf: 1.56M QPS median / 2.22M p99; <150 us median, <1 ms p99 (sec 4.4).
- Only for "selected namespaces" with deep/wide nesting; everything else stays normalized. Paper: "Zanzibar avoids storage denormalization... (except Leopard)" for consistency.

### 1.2 Caching / hot spots (sec 3.2.5, 4.4)
- Distributed cache of reads *and intermediate check results*, consistent-hashed (forwarding key usually = object id so related sub-checks land on one server); caller+callee caches form cache trees; Slicer for hot-key redistribution.
- Snapshot timestamp is part of the cache key; evaluation timestamps are *quantized* (1s or 10s) so most requests share a snapshot and cache entries. Valid because Spanner snapshot read at T sees all writes <= T.
- Lock table (singleflight) per cache key: only one request computes, the rest wait. Hit rates are low but useful: delegate-side check cache 10% + 12% lock table; read cache 24% + 9%. Prevents ~500K internal RPCs/s.
- Hot objects: detected by outstanding-reads counter; read and cache *all* tuples of <object#relation> (trade bandwidth for cacheability). Super-hot groups: pre-read full member set (0.1% of groups).
- Delay eager cancellation of sub-checks when others wait on their lock entry.
- Result: 20M read RPCs/s to Spanner from ~200M cache lookups/s; median 1.5 rows/RPC.
### 1.3 Hedging (sec 3.2.7)
- Hedge Spanner and Leopard reads (send 2nd after Nth-percentile delay estimated online); 1% of Spanner reads (200K/s) benefit. NOT hedged between Zanzibar servers (would duplicate expensive checks).
### 1.4 Zookies (sec 2.2, 3.2.1)
- Content-change zookie = snapshot timestamp at write; checks pass zookie as "at least as fresh". Default staleness chosen as lowest "safe" value by tracking out-of-zone read probability (binomial CI). Safe (zookie >10s old) requests are ~100x more frequent than Recent.
- Config (namespace) files drive which relations use Leopard / denormalization.

### Postgres mapping (Leopard / flattening)
| Technique | Postgres form | Cost model | Consistency | Pain points addressed |
|---|---|---|---|---|
| GROUP2GROUP closure | Closure table `group_closure(ancestor, descendant)` PK(ancestor, descendant) + index (descendant, ancestor); maintained by AFTER triggers on the tuple table (or a deferred constraint-style trigger) | Read: 1 indexed lookup replaces recursive CTE. Write: edge add = |anc(parent)+1| x |desc(child)+1| rows; Zanzibar saw tens of thousands per tuple. Memory: O(#pairs). Counting-based maintenance (store path count) makes deletes cheap vs recompute | Same transaction as the tuple write => atomic, no incremental/offline merge layer, no zookies | P2 (no recursive CTE in policy), P3 (all lookups are PK/index scans), P1 (policy becomes plain EXISTS on 2 tables) |
| MEMBER2GROUP direct only | plain tuple table index on (subject, relation) | O(1) write | n/a | P3 |
| Leopard intersection | Join `member2group(user) JOIN group_closure(group)` -> planner picks nested loop/merge on sorted btree = skip-list analog. For big sets: roaring bitmaps (pg_roaringbitmap) per (set) with `&` operator | bitmap: compact, O(min) intersect, but rewrite-on-update of the whole bitmap (TOAST) | bitmap updates serialise per set row => hot-group contention | P2 |
| Offline builder + incremental layer | Not needed when closure is in-DB trigger-maintained. Use only if write amp is too high: build closure in bg worker, keep a small "delta" table merged in view | adds merge cost to every read | needs a version/visibility rule | -- |
| Restrict to "selected namespaces" | Only maintain closure for relations flagged recursive/deep in the model; compile everything else to plain joins | avoids global write amp | -- | -- |

Decision rule worth copying: flatten only the group->group (transitive) edge, keep user->group direct. This is Zanzibar's own cost balance.

### Postgres mapping (cache / hot spot / hedging / zookies)
- Distributed consistent-hash caches: not applicable in-process per backend. Postgres equivalents: (a) per-backend memo of "set of group ids for current subject" computed once per statement/transaction (initplan or custom-scan materialization; Zanzibar's "read and cache all tuples of hot relation"), (b) shared-memory LRU keyed by (subject, relation, snapshot-xmin-quantum) -- caveat: must key on snapshot or tuple-table xmin/ commit counter, otherwise visibility bugs. Optional; closure table already makes most checks 1 index probe.
- Singleflight/lock table: skip (backend-local queries don't stampede the same way; pg buffer cache already shares pages). Hot-group contention is on *writes* instead (closure updates) -- mitigate by batching in deferred triggers.
- Hedging: N/A in single node; only relevant for read replica fan-out.
- Zookies/quantized snapshots: N/A in the same-DB design (MVCC gives it). Matters only for mirrored external tuples (SpiceDB/OpenFGA sync): store source revision per tuple batch and expose `pg_auth.synced_through()`.

---------------------------------------------------------------------------
## 2. SpiceDB internals
### 2.1 Dispatch + caching (VERIFIED)
- https://authzed.com/blog/spicedb-architecture : Dispatch interface = split request into sub-requests, cache sub-results, route sub-requests to instance likely to hold cache, handle leaf. Consistent-hash load balancing "always send the same (sub-)requests to the same server". Caching dispatcher can sit server-side or client-side. Hedging proxy datastore (second request if first statistically slow). No latency numbers published there.
- https://authzed.com/blog/hotspot-caching-in-google-zanzibar-and-spicedb : cache key = resource#relation@subject@revision (immutable entries); revision quantization default 5s; `--datastore-revision-quantization-max-staleness-percent` blends adjacent revisions so the whole cache doesn't evict at each boundary; consistency modes: fully_consistent (cache effectively disabled), at_exact_snapshot, minimize_latency, at_least_as_fresh (quantized but >= ZedToken).
- Changelog (https://github.com/authzed/spicedb/blob/main/CHANGELOG.md, first 100K chars only read): 1.57.0 experimental full-relation cache (serves reads incl. empty answers with no datastore query); 1.54 singleflight in DispatchQueryPlan; Otter cache; 1.56 compiled-caveat cache per schema version.
### 2.2 LookupResources v1 -> v2 -> v3 -> query planner (VERIFIED via changelog + README)
- LR2 experimental in 1.35.0; LR3 in 1.45.4 "based on new cursored iterator library"; 1.46.2 big LR3 dispatch speedups.
- Query planner (experimental flag `--experimental-query-plan`, 1.49-1.57): schema compiled to a *tree of iterators* (Datastore, Arrow, Union, Intersection, Exclusion, Alias, Caveat, Recursive) each implementing Check / IterSubjects / IterResources; results streamed lazily as Go iter.Seq. Source: https://github.com/authzed/spicedb/blob/main/pkg/query/README.md
- Optimizations (https://authzed.com/blog/query-planner-early-benchmarks): arrow reversal (walk from the smaller side), branch reordering (likely-hit first for unions, most selective first for intersections), arrow rotation ((A->B)->C => A->(B->C)), reachability pruning (drop branches that cannot yield the subject type), statistics advisors (static cost + counts from past queries). Benchmarks on Postgres: local plan 1.9-2.7x faster than old dispatch; on in-memory DeepArrow(30 levels) 75 us vs 771 us. Dispatching plan fragments across nodes was *slower* than old dispatch (early; "over-dispatching").
- Takeaway: SpiceDB itself is converging to "a relational-style planner with cost stats" -- i.e. what Postgres already has. Argues for compiling to SQL and letting the PG planner do reorder/selectivity, rather than re-implementing graph walking.
### 2.3 Materialize (permission-set denormalization) (partly SNIPPET; AuthZed Dedicated only)
- https://authzed.com/blog/authzed-materialize-generally-available : pre-computes "selected transitive relationships" and expensive permission queries; periodic snapshot recompute + live change stream folded in; "single-digit ms"; billions of relationships for select customers since 2024. No update-cost figures published (VERIFIED absence).
- API design (https://buf.build/authzed/api/docs/main:authzed.api.materialize.v0, SNIPPET): WatchPermissionSets emits ADDED/REMOVED edges (parent set -> child member, which may itself be a set) instead of per-(resource,subject) permissions. Rationale: WatchPermissions explodes when a role is granted to a huge team ("several orders of magnitude fewer changes" with sets); WatchPermissions deprecated. Consumer does the final JOIN: permission_sets table JOIN resources JOIN subject (Go client docs). LookupPermissionSets = backfill, then Watch from backfill revision (cursored).
- This is Leopard's two-level idea productised: store set->set edges (closure of sets) and set->member edges separately, and intersect at read.
### Postgres mapping
- Permission-set denormalization = tables: `perm_set_edge(parent_set, child_set_or_member)` + `resource_set(resource_id, set_id)`; policy = `EXISTS (SELECT 1 FROM resource_set rs JOIN set_closure sc ON ... WHERE rs.resource_id = t.id AND sc.member = current_subject)`. Update cost is per set edge, not per (resource x subject). Maintain via triggers (small) or bg worker draining a change log (large). Reads: indexed joins, P2/P3 friendly. Key insight: *share* sets among resources (e.g. all docs in folder F share set "F#viewer") so a role grant touches 1 set edge, not N resources.
- Partitioning (P4): resource_set can be partitioned by resource type/tenant; closure kept un-partitioned or partitioned by tenant. Policy quals must include the partition key (tenant) to prune.

---------------------------------------------------------------------------
## 3. OpenFGA performance work
Sources: https://openfga.dev/blog/weighted-graph-upcoming-changes (VERIFIED, no benchmarks given); https://github.com/openfga/openfga CHANGELOG + internal/check/*.go (VERIFIED); Auth0 ListObjects blog via https://daily.dev/posts/inside-openfga-s-improved-listobjects-algorithm-0q9jrsmoe (summary only).
- Weighted graph: built at model-save time; each edge/node gets weight = (roughly) min number of DB queries / traversal depth to the terminal type; request path chosen from weights; models failing to build are rejected at write time; replaces depth-limit 25 with throttling/flattening/ctx cancellation. Recursion only for pure-union relations (recursion with `and`/`but not` rejected). Short-circuit on intersection/exclusion. (DeepWiki summary: https://deepwiki.com/openfga/openfga/2.3-performance-optimizations, secondary)
- Check v2 strategies in code (internal/check/): default, `weight2` (intersect two *sorted* producer channels: ReadStartingWithUser vs Read for TTU; ordering-based pruning discards values that can never match -- i.e. merge-join), `recursiveTTU` (BFS: load terminal type, expand level by level with a visited set -- it is a recursive CTE), and a planner that picks strategy by Bayesian (alpha, beta, lambda priors) latency estimates per strategy (strategies.go).
- ListObjects "pipeline": DFS-built concurrent worker pipeline (numProcs, bufferSize, chunkSize); dedup + batching per node; edge pruning (#3075), improved intersection (#3031). Still graph-walking from the datastore.
- Caches (off by default): check query cache (subproblem results), check iterator cache (tuple reads), cache controller invalidates by last-write time; TTL + jitter issue (#3029). Third-party issue notes iterator cache is smaller win than result cache, doesn't help first-hit (https://github.com/openfga/openfga/issues/3094).
- Known pain: tuple-reader fan-out for dual-direction recursive inheritance (issue #3094) -- unsolved at scale without flattening. OpenFGA has no Leopard-style closure (UNVERIFIED that none is planned).
### Postgres mapping
- Weighted graph => compile-time model analysis: compute for each permission the join depth / recursion points; reject or warn on models whose compiled SQL exceeds budget; choose per relation among {direct join, closure-table lookup, recursive CTE}. Cheap, pure-extension logic, no runtime cost.
- weight2 => ordinary merge join / semi-join; ensure indexes exist so planner can choose it: tuple table indexes (object_type, relation, object_id, subject_*) AND reverse (subject_type, subject_id, relation, object_type, object_id). pg_auth should *create/verify* these (P3).
- Recursive-union-only restriction => only these compile to `WITH RECURSIVE` or closure; and/but-not must be non-recursive. Same restriction is a good v1 rule.
- Strategy planner by latency stats => PG planner statistics; supply `pg_statistic` quality via extended statistics / custom `SupportFunction` rows estimates rather than own bandit.

---------------------------------------------------------------------------
## 4. In-house systems
| System | Source | What they do | Numbers | Precompute vs read | List/filter | Notes |
|---|---|---|---|---|---|---|
| Airbnb Himeji | https://medium.com/airbnb-engineering/himeji-a-scalable-centralized-system-for-authorization-at-airbnb-341664924574 (403 via fetch; details from InfoQ https://www.infoq.com/news/2021/05/airbnb-himeji + search snippet) | Zanzibar-inspired tuples entity#relation@principal; YAML set-algebra config; sharded replicated in-memory cache in middle layer over Aurora; SpinalTap CDC -> Kafka -> cache invalidation; checks live in data services | 850k entities/s (Mar 2021), p99 12 ms, 99.999%, ~98% cache hit target | compute-on-read + cache, invalidate on change | in-memory filter in cache layer only; no list/denormalization detail (VERIFIED absence in InfoQ) | Lesson: put check next to data. pg_auth is the extreme version. |
| Carta AuthZ | https://medium.com/building-carta/authz-cartas-highly-scalable-permissions-system-782a7f2c840f (403; SNIPPET only, UNVERIFIED details) | Zanzibar-style; stores graph in Postgres; "Flatten" API flattens permission graph into an index; consumers use Flatten output to query *their own* DB; list lookups fast with index but slow with extra filters => added trigram index on top of the custom index; secondary index for path-exists checks | "<10 ms" in title of part 2 (https://medium.com/building-carta/user-authorization-in-less-than-10-milliseconds-f20d277fec47) | precompute (flatten) | flatten -> `id IN (...)` / join in consumer DB | Closest real-world analog: Postgres-backed flattened index; learned that filter+authz combination needs its own index design. Verify directly before citing. |
| Figma PermissionsV2 | https://www.figma.com/blog/how-we-rolled-out-our-own-permissions-dsl-at-figma/ (VERIFIED) | JSON boolean-expression policies (allow/deny, applyFilter), evaluated in TS/Ruby/Go with shared tests; lazy data loading with 3-valued logic (null = undetermined), sequential batches ordered by heuristics (file, folder, team roles first); short-circuit | perms checks were ~20% of DB load; short-circuit + lazy load >halved eval time. No latency/QPS numbers | compute-on-read | not covered | Lesson for pg_auth: evaluate cheap/likely-decisive clauses first, deny-first; 3-valued evaluation maps to SQL NULL; Postgres planner won't reorder OR-branches by cost across function calls, so order/shape the generated qual (cheap `COST` on functions, bool-expr ordering). |
| Facebook TAO / privacy | https://www.usenix.org/system/files/conference/atc13/atc13-bronson.pdf (VERIFIED text) | Privacy checks run per viewer at view time; paper explicitly says aggregation/filtering can't be done at creation time and privacy-check results are NOT cacheable across viewers; TAO caches nodes/edges only; 1B reads/s; 0.2% writes | assoc_count: 1% of counts >=512K | compute-on-read (authz); precompute only underlying graph | filter after fetch (post-filter) per item | EntSchema privacy rules: ordered Allow/Deny/Skip rules per entity (https://ent.dev/docs/core-concepts/privacy-policy) -- app-layer, post-filter. Not a model for DB-level filters. |
| Slack, Notion, Netflix, Lyft, Dropbox, Canva, Pinterest, Uber, GitHub | searched | No primary technical authz write-ups found with numbers (Slack Flannel is a metadata cache, not authz; Notion walks parent pointers to root per block -- third-party, UNVERIFIED). Marked UNVERIFIED/not found. | | | | Do not cite. |

---------------------------------------------------------------------------
## 5. List filtering guidance
- AuthZed (https://authzed.com/docs/spicedb/concepts/querying-data, VERIFIED): LookupResources (pre-filter) is "heavy" above ~10k results; then use CheckBulkPermissions post-filter; then Materialize; LookupSubjects has no cursor; ReadRelationships does no caching and may full-scan.
  Protecting-a-list-endpoint doc: https://authzed.com/docs/spicedb/modeling/protecting-a-list-endpoint (not fetched).
- Oso (https://www.osohq.com/docs/develop/enforce/list-filtering, VERIFIED): centralized list returns ids, best up to ~10,000 authorized resources per user; `list_local` returns a SQL WHERE fragment over your own tables (data bindings map facts to tables) and targets millions of resources/user, supports DB pagination, joins, aggregations. Oso evaluates centralized facts and leaves local relationships as SQL. Tradeoff: policy coupled to DB schema. https://www.osohq.com/post/local-authorization
- Convergent finding: every vendor ends at "ship a predicate/SQL to the data" or "denormalize into the data store". Validates pg_auth's compile-to-SQL thesis, and gives the thresholds: ~10k ids is the ceiling of `id = ANY($ids)`.
- Aserto/Permit.io: not researched (UNVERIFIED).

---------------------------------------------------------------------------
## 6. Technique ranking for pg_auth, judged vs P1-P4 and DB-enforced/no-app-layer

1. **Closure table for transitive group/folder edges (Leopard GROUP2GROUP), trigger-maintained, only for relations flagged recursive in the model.** Kills recursion (P2), makes everything an indexed point/range lookup (P3), policy is a plain EXISTS with no functions (P1). Same-txn consistency removes Leopard's offline+incremental complexity. Risk: write amplification (Zanzibar: tens of thousands rows per tuple) => counted-paths maintenance, deferred batched triggers, cap/guard in schema validation.
2. **Shared permission-set indirection (Materialize permission sets / Leopard two-set design).** Resources point to a *set*, sets point to sets/members; a grant touches one set edge. Keeps the policy to 2 indexed joins, keeps write cost proportional to set edges rather than resource x subject. Partitioning-friendly if set id carries tenant.
3. **Index generation/verification + SQL shape discipline (OpenFGA weight2/merge-join, SpiceDB arrow reversal/branch ordering/pruning).** Auto-create forward and reverse tuple indexes, per-resource `(resource_id)` and `(subject)` indexes on denormalized tables, generate semi-joins not scalar subqueries, prune branches by subject type at compile time, order cheap quals first, mark any helper functions IMMUTABLE/STABLE with COST + inlinable SQL language (P1). No runtime machinery. Highest effort/benefit ratio.
4. **Compile-time model weighting/validation (OpenFGA weighted graph).** Reject/warn on models whose SQL would have >N joins, mixed recursion with and/not, unbounded depth. Pure compile-time.
5. **Per-statement subject-set materialization (Zanzibar hot-object "read all tuples and cache").** Compute subject's group set once per statement (initplan / custom scan), then probe `= ANY(array)` or hash semi-join; fall back to join when set > ~10k (Oso/AuthZed threshold). Fixes SubPlan-per-row.
6. **Custom scan / cost + selectivity hooks** (SpiceDB stats advisors; OpenFGA latency-bandit): supply row estimates for the injected predicate so the planner doesn't choose a seq scan (P3). Needs extended stats on tuple/closure tables.
7. **Bitmaps (roaring) for very large sets.** Optional; only if closure rows explode; update contention on hot sets.
8. **Shared-memory result cache keyed on snapshot** (Zanzibar cache + quantization, SpiceDB). Lowest priority: closure makes checks cheap, and snapshot-keyed correctness is easy to get wrong. Quantization is only needed for mirrored external backends (sync revision bookkeeping).
9. Not worth reproducing: hedging, consistent-hash dispatch, lock tables, cache trees, zookies for same-DB tuples (MVCC covers).

Caveats: Zanzibar/SpiceDB numbers are for distributed services with network hops; our per-row cost model is different (index probe ~ microseconds; the risk is plan shape, not RPC count). Partitioned tables (P4): ensure injected quals carry the partition key or planner can't prune, and keep closure/set tables outside the partitioned heap or partition them with the same key. Carta, Himeji (primary page), Slack/Notion/etc. details are unverified as noted.
