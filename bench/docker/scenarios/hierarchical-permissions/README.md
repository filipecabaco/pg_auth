# hierarchical-permissions

Catalog issue 18. Files inherit access from ancestor folders. Naive policy: a correlated recursive CTE per file.
Fix: precomputed closure + array.

Data (scale = folders, default 50k): a 3-ary folder tree (depth 10 at 50k), 2 x scale files, scale/50 noise users
with 1-2 random grants each, `app.folder_closure` (505,719 rows at 50k, 35 MB) for the closure variants.
Subjects: `selective` (user 1; grants on folders 122 and 365, a subtree of a few hundred files), `broad` (user 0;
grants on folders 2 and 3, about 2/3 of the tree). Queries: `count`, `recent` (ORDER BY created_at LIMIT 20),
`one_folder` / `deep_folder` (list one folder).

Read results (scale 50k, 3 reps, medians): naive_rls selective `count` 684 ms (RecursiveUnion x100k loops) vs ~0.12-0.3 ms
for the closure array; broad `count` ~600 ms vs 9-11 ms. For `one_folder` with the broad subject naive wins (0.03 ms vs
3.5-6 ms) because the array variants must build a ~33k-element array per statement (issue 19).

`pg_auth_hierarchy` declares the tree (`hierarchy => 'app.folders(id, parent_id)', direct => 'viewer_direct'`):
OpenFGA lists only the subject's direct grants (the model's `viewer_direct`), and Postgres expands them through a
closure of `app.folders` kept by triggers. Same rows as the engine-backed variants, for every subject and query.
Measured 2026-10-28 (2 CPUs, 2 GB; OpenFGA on Postgres, caches on): warm medians match `pg_auth` with a fresh
permission index (geomean 0.61x vs 0.62x of no RLS; broad `count` 8.0 ms vs 7.9 ms). The difference is the cold
statement: the broad subject's first `count` in a new backend takes 22 ms (2 grants listed, ~33k folders expanded)
against 6.4 s through the engine (waiting for its permission index fill), and a tree change (moving folder 4 and its
subtree: 48 ms in the trigger) is visible to the next transaction with one 22 ms expansion, not a refill.
