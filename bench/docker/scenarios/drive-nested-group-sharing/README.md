# drive-nested-group-sharing

Google-Drive/Notion-style sharing. Folders form a tree (documents live in folders); a folder can be shared
with a user or with a group; groups contain users and other groups (6 levels); a share applies to the
whole subtree. Access to a document = a share on its folder or any ancestor, to the user or to any group
the user belongs to directly or through nesting.

Subjects: `direct` (one user share on folder 100, no group), `deep_group` (leaf group 6 levels deep, shares
on it and 4 ancestor groups), `big_group` (group 3 owns folder 2: a quarter of the tree).
Scale = documents (default 50k, 1000 folders, 364 groups). 10x = 500k documents, 10k folders,
closure ~70k rows; OpenFGA tuples ~2k -> ~25k (folders, groups, memberships, shares, none per document).

## Why RLS is good / bad here
- Two graphs must be walked: folder ancestors and group parents. A policy cannot select from `folders`
  while `folders` has its own policy ("infinite recursion detected in policy"), so the walk goes in a
  security definer function. Per row (`naive_rls`) that is a recursive CTE per document: ~280 ms for a
  count on 50k documents against 0.1-2 ms for the others.
- `recursive_fn_rls`: definer function expands groups + subtree once per statement (InitPlan array).
  Fast when the visible set is small; it pays the whole expansion even for a point lookup and grows with
  the visible folder set (10x run will show it).
- `best_practice_rls`: closure tables `folder_closure`, `group_ancestors` make reads cheap and move the
  cost to writes: moving a folder rewrites its subtree's closure rows; adding a group to a group rewrites
  ancestor/descendant pairs.
- Sharing a single document is not modelled: protecting `documents` by `folder_id` keeps tuples at folder
  level. Per-document shares would need a document object (one tuple per document) or a second column;
  pg_auth's one-column mapping makes that the cost.

## pg_auth
`group.member: [user, group#member]` gives nesting, `folder.viewer: [user, group#member] or viewer from
parent` gives inheritance. `folders.id` and `documents.folder_id` are protected on `folder:viewer`.
No closure tables, no function.

## Which index each query uses
point: `documents_pkey`. list_folder: `documents_folder_updated (folder_id, updated_at desc, id)`.
subfolders: `folders (parent_id, id)` + `documents_folder_updated`. count/tree_count: scans.
recent: `documents_updated (updated_at desc, id)` walked until 20 visible rows. search: `ilike '%x%'`, none.
Permission side: `folder_shares` partial indexes on `(user_id, folder_id)` / `(group_id, folder_id)`,
`group_users (user_id, group_id)`, `group_groups (child_id, parent_id)`, closure tables both ways.

## Sources
- Recursive RLS policy vs precomputed ancestors measured on 2M files (13.8 s vs 152 ms): https://github.com/rowstile/rowstile/pull/84
- Supabase RLS performance recommendations: https://supabase.com/docs/guides/database/postgres/row-level-security (sections "Add indexes" and "Call functions with select")
- Infinite recursion in policies and definer-function fix: https://github.com/orgs/supabase/discussions/47525
- Nested groups as OpenFGA usersets (`group#member`): https://openfga.dev/docs/modeling/building-blocks/usersets-and-groups (not fetched; from OpenFGA docs)
