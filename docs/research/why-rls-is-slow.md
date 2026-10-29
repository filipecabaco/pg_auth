# Why RLS is slow

Status: research · 2026-10-27. The analysis behind pg_auth: where RLS spends its time, measured
on the policy shapes people write. What pg_auth does about it is in [design.md](../design.md); its
results against these shapes are in [the benchmarks](../benchmarks.md).

**Constraint:** enforcement must live in the database for every client. Moving checks into an
application layer, the usual escape from slow RLS, is out of scope.

Inputs:
- the benchmark in [`bench/`](https://github.com/filipecabaco/pg_auth/tree/main/bench), run on Postgres 18.6 (Homebrew, Apple silicon);
- two prior-art surveys: [`research/scaling-db-native.md`](scaling-db-native.md) and
  [`research/scaling-authz-services.md`](scaling-authz-services.md). Claims in the
  surveys marked [U] or UNVERIFIED have not been confirmed.

## 1. What makes RLS slow

The RLS mechanism itself costs nothing measurable. The cost comes from four places.

1. **Policy subqueries are never turned into joins.** `planner.c` runs `pull_up_sublinks` on the
   join tree only, and `rte->securityQuals` are preprocessed afterwards (`planner.c:1023` on
   REL_17). An `IN`/`EXISTS` in a policy therefore becomes a SubPlan, either per row or hashed,
   never a semi-join. That rules out:
   - join reordering;
   - parameterized nested loops;
   - run-time partition pruning driven by the join.
2. **Functions in policies run per row** unless they are wrapped as an InitPlan,
   `(select f())`. PL/pgSQL is never inlined, and neither are `SECURITY DEFINER` or `SET`
   functions.
3. **Security-barrier ordering.** A user predicate that is not leakproof cannot be pushed below
   the policy predicate or used as an index condition (`indxpath.c`,
   `restriction_is_securely_promotable`). Comparison operators on `int`, `bigint`, `text`,
   `uuid` and `timestamptz` are leakproof. These are **not**:
   - `LIKE`/`ILIKE` (`~~`, `~~*`) and `~`;
   - every `jsonb` operator and `@@`;
   - comparisons on `numeric`.
4. **Estimates.** The planner doesn't know how many rows a given subject can see, so
   `ORDER BY … LIMIT` and later joins get planned for the wrong cardinality.

Partitions add one correctness trap. Parent policies apply only when the table is queried
through the parent. Querying a partition directly applies only that partition's own policies,
so every leaf partition needs protecting too.

## 2. Benchmark

Setup:
- `documents`: 1M rows, 10k orgs, hash-partitioned copy `documents_p` with 16 partitions;
- `comments`: 1M rows, each visible only if its document is visible (a nested policy);
- `memberships`: has its own RLS;
- two subjects: **small** sees about 200 documents (2 orgs); **power** sees about half the
  table (5,000 orgs);
- parallel query off, median of 3 runs, warm cache.

Values are median execution time in milliseconds.

| query | user | plpgsql fn | SQL secdef fn | correlated EXISTS | IN subquery | `= ANY(array(...))` | security-barrier view + IN | **flat join (no barrier)** |
|---|---|---|---|---|---|---|---|---|
| pk lookup | small | 0.04 | 0.04 | 0.01 | 0.02 | 0.02 | — | 0.01 |
| count | small | 2288 | 1252 | 41 | 26 | 0.02 | 0.06 | 0.02 |
| latest 20 (`ORDER BY created_at LIMIT`) | small | 247 | 133 | 5.9 | 6.0 | 4.5 | 0.49 | **0.08** |
| unindexed `LIKE` | small | 2502 | 1355 | 43 | 43 | 0.06 | 0.45 | 0.06 |
| comments (nested policy) | small | 2524 | 1440 | 147 | 145 | 30 | 0.83 | **0.43** |
| partitioned count | small | 2479 | 1390 | 43 | 43 | 0.08 (16/16 parts) | 0.20 (2/16) | 0.04 (2/16) |
| partitioned latest 20 | small | 2234 | 1252 | 57 | 57 | 44 (16/16) | 0.53 (2/16) | **0.17 (2/16)** |
| count | power | 2315 | 1302 | 66 | 48 | **22** | 68 | 64 |
| latest 20 | power | 0.11 | 0.08 | 0.53 | 0.56 | 0.61 | 111 | **0.04** |
| unindexed `LIKE` | power | 282 | 160 | 8.7 | 8.7 | 12 | 21 | 6.8 |
| comments (nested policy) | power | 2869 | 1703 | 333 | 332 | 285 | 711 | 361 |
| partitioned count | power | 2688 | 1473 | 93 | 96 | **49** | 115 | 106 |
| partitioned latest 20 | power | 0.24 | 0.16 | 7.7 | 8.6 | 0.77 | 154 | **0.08** |

The "flat join" column runs as superuser with the permission check written into the query, as
an app-layer filter would be. It is the target to beat, not a candidate mechanism.

### Reading the results
- **Functions are the first-order problem.** A per-row function costs about 1–2.5 s against
  0.02 ms for the same logic as an InitPlan array, roughly 100,000×. Supabase's published numbers
  show the same thing.
- **`= ANY(array(...))` is the best shape plain RLS can reach.** It is indexable and evaluated
  once. Even so, a selective subject is still 30–260× slower than the flat join on:
  - nested policies;
  - `ORDER BY … LIMIT`;
  - partitioned tables, where it never prunes, even outside RLS.
- **No single static shape wins.**
  - Array or filter shapes win for broad subjects; the planner walks the `created_at` index and
    stops after 20 rows.
  - Join shapes win for selective subjects; the planner drives from the permission set.
  - A security-barrier view with an `IN` gets the join for the small user, but loses badly for
    the power user (111–154 ms). The barrier subquery is planned without knowing the outer
    `LIMIT`, and this happens even with the subject baked in as a constant.
- **The flat join is best or close to best in every row.** This is why teams move authorization
  into the application. The task is to get the flat-join plan while staying DB-enforced.
