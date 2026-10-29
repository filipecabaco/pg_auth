# Proposal: attribute rules from Cerbos

Status: phases 1–3 done; planning cost of compiled filters open · 2026-10-29

## Why

OpenFGA and SpiceDB answer one question about a row: may the subject access the object the row
names (its id, or a parent's)? Many rules people write as RLS are about the row itself instead:
`owner_id = auth.uid()`, `status = 'published'`, a role from the token that sees every row of
a tenant. Cerbos is an engine for exactly those rules: stateless, policies in YAML with CEL
conditions over the principal and the resource's attributes, Apache-2.0. It stores no
relationships, so it cannot list a subject's objects; instead its PlanResources answers, for a
principal, a resource kind and an action: always allowed, always denied, or a condition over the
resource's attributes. That condition, compiled into SQL over the row's columns, is a filter the
planner can see: an index on `owner_id` serves `owner_id = 'anne'` as it serves RLS.

## Model

- **A row is a resource.** `authz.protect(table, type => kind, id => id column, read =>
  action, attributes => array['owner_id', 'status'])`: the resource's kind, its id, and the
  columns sent as its attributes, by their names. Only those columns are visible to the policy.
- **The subject is a principal.** Its id is the subject's id (`user:anne` gives `anne`); its
  roles come from the request (a claim, `authz.configure(roles_claim => 'app_metadata.roles')`,
  default `role`, which Supabase sets to `authenticated`; or the setting `pg_auth.subject_roles`
  for `subject => 'guc'`); its attributes are the token's claims.
- **The protocol gains a principal and a plan.** Requests may carry `"principal": {"roles": [...],
  "attr": {...}}`, which engines that need none ignore. A new operation:

      {"op":"plan","subject":"user:anne","principal":{...},"type":"document","permission":"read"}
        -> {"plan":"always"} | {"plan":"never"} | {"plan":"condition","condition":<expr>}

  `<expr>` is pg_auth's own small expression language, not Cerbos's, so a later engine (OPA's
  compile API) can produce it too: `{"op":"and|or|not|eq|ne|lt|le|gt|ge|in","args":[...]}`, an
  argument being `{"column":"owner_id"}`, `{"value":...}` or a nested expression. The sidecar
  translates Cerbos's plan; anything outside this language (arithmetic, lambdas, `has`) makes the
  plan `{"plan":"rows"}`: check every row with its attributes. `check` gains `"attrs"`, one
  object per id, which Cerbos's CheckResources needs (50 resources a call).

## Enforcement, in phases

1. **Per row, without the planner.** The policy on a protected table is
   `authz.permits_resource(kind, action, id::text, jsonb_build_object('owner_id', owner_id, ...))`.
   It fetches the subject's plan once per statement and evaluates it in Rust against the row's
   attributes: no round trip per row. A plan of `rows` checks each row with the engine. Correct
   for every subject and every cached plan, since nothing is decided at planning. Fails closed.
2. **The plan as a planner-visible filter.** The planner hook replaces that policy with the
   compiled condition, so indexes and partition pruning apply. No subject-specific constant may
   sit in a cached plan: every value that came from the principal becomes an InitPlan
   `(SELECT authz.plan_value(kind, action, n))`, read at execution for whoever runs it. The
   condition's *shape* also depends on the principal (an admin's plan is `always`, a user's a
   comparison), so the filter is guarded: `CASE WHEN (SELECT authz.plan_shape_is(kind, action,
   <hash>)) THEN <condition> ELSE <phase 1 policy> END`. A cached plan reused by a principal of
   another shape answers correctly through the per-row path, and is replanned at the end of the
   statement, as plans of another breadth are today.
   As built: the hook reads the subject's plan as a skeleton (`authz.resource_plan`: values
   replaced by numbered placeholders with their kind), compiles comparisons, `in`, `and`, `or`,
   `not` over the attribute columns and the id, each value `(SELECT authz.plan_value(kind,
   action, n, column type))::type` (NULL when the value is not valid input for the column's
   type: the comparison is unknown, never an error), and ANDs `(SELECT authz.plan_shape_is(kind,
   action, skeleton))`, which raises (SQLSTATE 40001, "run it again") if the running subject's
   plan has another shape. A value of another kind than its column (a string against an integer)
   compiles to unknown, as condition.rs evaluates it; strings order by code point (`COLLATE "C"`)
   as Cerbos orders them, equality keeps the column's collation and its index. Shapes are compared
   at the end of every statement and at a SET inside one (as breadth classes are), and a top-level
   SET of a setting the subject comes from (no snapshot to compare with) discards the plans holding
   compiled filters at once; so the guard is a safety net. Compiled only with
   `pg_auth.replan_on_breadth` on; `rows` plans keep the per-row policy.
3. **Harness.** A Cerbos adapter for the compare harness (Cerbos v0.57 in a container, policies
   from the scenario's `pg_auth/cerbos/*.yaml`), Cerbos variants of the attribute scenarios
   (`supabase-owner-or-role`, `supabase-claims-rbac`, `rls-best-case-tenant-column`), and a
   parity check: random rows and principals, the rows each sees against CheckResources.

## Results (2026-10-29)

- **Parity** (`compare/attributes.exs`, against CheckResources): every principal matches in the
  compiled, cached-plan and per-row modes; seeds 1, 3 and 7, up to 2000 rows and 200 principals.
  It caught two bugs: CEL's equality is defined for null and mixed kinds (`null != "draft"` is
  true), which the first compiled form and the evaluator treated as unknown; and Postgres's hashed
  `= ANY` keeps pointers into the array it read on the first row, which for a short InitPlan array
  of a by-reference type (text, uuid) is a copy in that row's memory: hashing is now for by-value
  types only, which also fixes the listed-ids path on text and uuid columns.
- **Benchmarks** (`pg_auth_cerbos` and `pg_auth_cerbos_reused_plan` variants; planning plus
  execution against best-practice RLS): `supabase-owner-or-role` 1.44x (pg_auth on OpenFGA
  0.96x), with the selective user at 0.04 ms where RLS scans (1–3 ms); `rls-best-case-tenant-column`
  4.4x (pg_auth 2.5x), RLS's best case. Execution is close to pg_auth's and partition pruning
  applies; the gap is planning, 0.1–0.2 ms per protected table (the skeleton read through SQL, the
  filter built with catalog lookups) and the InitPlans, on statements that run in 0.01–0.1 ms.
  Plans are kept across transactions (the sidecar's `valid_ms`) per principal, keyed by a hash of
  its content (not by when it was resolved: a request's subject comes and goes with its
  transaction-local settings), and the skeleton is computed in Rust when the subject is resolved:
  planning 0.6–2 ms, then 0.2 ms, now 0.11 ms (tenant scenario 3.0x, pg_auth on OpenFGA 2.4x).
  What remains is common to every engine (design.md, open question 6): prepared internal queries
  and `authz.warm()` cut a new connection's first planning (0.4 -> 0.15 ms) but not steady state;
  an idle backend whose kept plan expired (half the sidecar's 1 s TTL) waited on Cerbos at
  planning (+2.2 ms) until the sidecar refreshed active plans in the background
  (`REFRESH_ACTIVE_MS`): 2.65 -> 0.47 ms after 1.5 s idle. With it, `supabase-owner-or-role`
  0.83x of best-practice RLS (was 1.16x; median planning 0.17 -> 0.10 ms), tenant column 2.8x.

## Risks

- **Semantics of NULL.** CEL on a missing attribute errors and the rule does not apply; SQL's
  comparison with NULL is unknown, which a filter treats as false. Equal for allow rules; a
  `not` over a comparison with NULL must stay false (`coalesce(..., false)` around comparisons).
- **Types.** Values arrive as JSON (numbers, strings, booleans); comparisons are compiled against
  the column's type, and a value that does not cast to it makes the comparison false, never an
  error that leaks which rows exist.
- **Numbers are doubles in Cerbos.** Attributes arrive as JSON, which Cerbos reads as doubles:
  `request.resource.attr.size + 1 > 10` has no CEL overload (double + int) and never applies;
  policies write `size + 1.0 > 10.0`. Comparisons (`size > 10`) are unaffected.
- **Policy changes.** Cerbos reloads policies from its store without a change feed; the sidecar
  caches plans for its answer TTL (1 s), and the extension per statement.
