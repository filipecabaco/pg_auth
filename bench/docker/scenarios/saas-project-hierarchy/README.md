# saas-project-hierarchy

Linear/Jira-style SaaS: orgs -> teams -> projects -> issues -> comments (+ labels). A project is owned
by a team and may be shared with extra teams or with guest users; org admins see every project of their
org. Supabase-style: users are uuids, policies use `auth.uid()`, subjects carry JWT claims.

Access to a project = org admin of its org OR member of its owner team OR member of a team it is
shared with OR guest of the project. Issues, labels, comments and issue_labels inherit from the project.

Subjects: `guest` (one project, ~1% of data), `member` (team 1 + shares, ~6%), `org_admin` (~91%).
Scale = issues (default 50k; 110 projects, 100k comments). 10x = 500k issues / 1M comments; OpenFGA
tuples stay ~1k (memberships, shares, guests) because every table is protected by `project_id`.

## Why RLS is good / bad here
- Good: with the project set computed once (security definer helper -> int[] in an InitPlan) and an index
  on `project_id`, issue/label reads are as fast as the explicit semi-join, for narrow subjects.
- Bad: tutorials write each policy as an EXISTS against the parent (comments -> issues -> projects ->
  4 membership lookups); Postgres runs it as a SubPlan per row, and a join across the three tables pays
  it on every table. `comments` and `issue_labels` only know `issue_id`, so even the best practice array
  policy probes `issues` per comment; the fix is a denormalised `project_id` per child table
  (`best_practice_denorm_rls`), which every write path must keep right.
- Broad subjects (org_admin): the array policy filters by `= any(100 ids)` and the `my_open` page
  (partial index `issues_my_open`, ORDER BY updated_at LIMIT 20) loses its early stop; the baseline's
  semi-join keeps it (see results).

## Variants
`no_rls_baseline` (explicit `project_id in (visible_projects(uid))`), `naive_rls` (nested EXISTS chain),
`best_practice_rls` (definer array helper; comments/issue_labels via issues), `best_practice_denorm_rls`
(project_id copied onto comments/issue_labels), and the four `pg_auth*` variants.

## pg_auth
Model: org.admin -> team.can_access -> project.viewer (guest, owner_team, shared_team) in
`pg_auth/model.fga`. `projects.id`, `issues/labels/comments/issue_labels.project_id` are protected on
`project:viewer`. Comments and issue_labels are protected by their denormalised `project_id`, because
protecting by `issue_id` would need one `issue -> project` tuple per issue (500k tuples at 10x); that is
a limitation of the one-column mapping, not of the model: pg_auth wants the deciding object in the row.
No tuples per issue or comment.

## Which index each query uses
point: `issues_pkey`. thread: `comments_issue (issue_id,id)`. board: `issues_project_status` (index-only).
my_open: `issues_my_open` (partial, assignee_id, updated_at desc). project_stats/label_stats: scans;
comment_counts: `issues` scan + `comments_issue`; busiest: scans + hash joins. search and comment_search:
`ilike '%x%'` on purpose, no index (a pg_trgm GIN would fix them in every variant alike).
Permission lookups: `team_members` pk + `(user_id, team_id)`, `org_admins` pk + `(user_id, org_id)`,
`project_team_shares` pk + `(team_id, project_id)`, `project_guests` pk + `(user_id, project_id)`.

## Sources
- Supabase RLS performance recommendations: https://supabase.com/docs/guides/database/postgres/row-level-security (sections "Add indexes" and "Call functions with select")
- GaryAustin1/RLS-Performance (benchmarks of wrapped functions, IN vs EXISTS): https://github.com/GaryAustin1/RLS-Performance
- "infinite recursion detected in policy" and definer-helper fix: https://github.com/orgs/supabase/discussions/47525 , https://www.guardlayer.io/blog/supabase-rls-infinite-recursion
- Membership subquery / array / STABLE-vs-VOLATILE measurements: https://now-next.nl/en/insights/row-level-security-performance-postgresql/
