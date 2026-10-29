# supabase-storage-paths

`storage.objects` with Supabase Storage's documented policies: the access rule is encoded in the
object's path, not in a column.

## The pattern

- `bucket_id` + `name` like `<user_id>/<project>/file.png`. Policies from the docs:
  `bucket_id = '...'`, `(storage.foldername(name))[1] = auth.uid()::text` (user folder), a team
  folder (`<team_id>/...`) resolved through `team_members`, and a public bucket.
- `storage.foldername()` is Supabase's own definition: `language plpgsql`, no volatility marking,
  so VOLATILE. It cannot be inlined and no index can serve it; the policy calls it once per row.
- Queries: `list_team_prefix`, `list_own_prefix` (the Storage API lists with prefix ranges),
  `count_mine` (`owner = auth.uid()`), `point_own` / `point_public` (one object by path),
  `denied_folder` (another user's folder: must return 0, the cost of saying no), `recent`.
- Subjects: `light`, `team_broad` (40% of teams), `heavy_owner` (4% of all objects in one folder).

## Why RLS is good / bad here

- Good: `list_team_prefix` and `point_public` are cheap in all variants: a bucket/prefix range is
  selective already and the policy only has to check a few rows.
- Bad: every query that is not narrowed by the policy's own columns (own folder, `count_mine`)
  calls `foldername()` per candidate row; the prefix index serves the app's range predicate, not the
  policy. Rewriting the rule with immutable `split_part` + expression indexes
  (`best_practice_indexed_rls`) fixes it, but that is not what the docs show.

## What pg_auth does, and the limitation

pg_auth cannot read the path: `protect` names one column holding an object id. Here that column is
`acl_key`, a stored generated column derived from the path (`public-assets`, or
`<bucket>_<first folder>`). The folder object has `owner`, `team_member` and `public` relations.
Findings: (1) the path rule is repeated in the column definition and in the tuples query, so a change
of folder convention must be made in both places (RLS keeps it in one policy); (2) object ids cannot
contain `:` or `#` in OpenFGA, hence `_` as separator; (3) one tuple per user folder owner and per
team membership, not per object.

## Indexes and what each query should use

- `list_*_prefix`, `point_own`: unique `(bucket_id, name)` / `(bucket_id, name collate "C")` range scan.
- `count_mine`: `objects_owner`. `recent`: `(created_at desc, id)`.
- Policy/pg_auth columns: `acl_key`; `(bucket_id, split_part(name,'/',1))` and the partial
  `split_part(name,'/',1)::int where bucket_id='team-files'` for `best_practice_indexed_rls`.

## Scale

`:scale` = objects (60% user folders, 20% team, 20% public); users = scale/50, teams = scale/500.
10x = 1M objects, 20k users, 2k teams. generate_series inserts only.

## Sources

- Storage access control: https://supabase.com/docs/guides/storage/security/access-control (its examples compare the folder with `auth.jwt()->>'sub'`; `auth.uid()::text` used here is the same value)
- Storage helper functions (foldername/filename/extension): https://supabase.com/docs/guides/storage/schema/helper-functions
- storage-api schema (function definitions, indexes): https://github.com/supabase/storage/blob/master/migrations/tenant/0002-storage-schema.sql

## Results (scale 100000, 3 reps, 2 CPU / 2 GB, median ms, shared host so indicative)

| subject / query | baseline | naive_rls | best_practice_rls | best_practice_indexed_rls | pg_auth | pg_auth_per_row |
|---|---|---|---|---|---|---|
| light / list_own_prefix | 0.018 | 89 | 58 | 0.19 | 1.2 | 16 |
| light / count_mine | 0.088 | 135 | 80 | 8.7 | 9.5 | 17 |
| team_broad / count_mine | 0.084 | 1834 | 86 | 12.7 | 11.8 | 24 |
| light / point_own | 0.014 | 92 | 67 | 0.15 | 1.0 | 18 |
| heavy_owner / list_own_prefix | 0.046 | 2.0 | 1.6 | 3.8 | 4.4 | 20 |

pg_auth is ~100x faster than the documented policies but 5-70x slower than the explicit filter on
own-folder lookups (a permission-index join where the filter is a single equality); the immutable
split_part rewrite is the only RLS shape that competes. All variants returned identical results.
