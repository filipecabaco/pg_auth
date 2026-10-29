-- :scale = number of issues. Fixed shape: 2 orgs, 22 teams (20 in org 1, 2 in org 2), 5 projects per
-- team (110 projects), 6 labels per project, 2 comments per issue, ~2 labels per issue.
-- Users are uuids (Supabase auth.users style); user n is app.u(n).
select :scale::int as ni, (:scale * 2)::int as nc, greatest(:scale / 200, 50)::int as nu \gset

create schema if not exists auth;
create or replace function auth.uid() returns uuid language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim.sub', true), ''),
                  (nullif(current_setting('request.jwt.claims', true), '')::jsonb ->> 'sub'))::uuid $$;
create or replace function auth.jwt() returns jsonb language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim', true), ''),
                  nullif(current_setting('request.jwt.claims', true), ''))::jsonb $$;
create or replace function auth.role() returns text language sql stable as $$
  select coalesce(nullif(current_setting('request.jwt.claim.role', true), ''),
                  (nullif(current_setting('request.jwt.claims', true), '')::jsonb ->> 'role'))::text $$;

create schema app;
create function app.u(n int) returns uuid language sql immutable as
  $$ select ('00000000-0000-4000-8000-' || lpad(to_hex(n), 12, '0'))::uuid $$;

create table app.orgs(id int primary key, name text not null);
insert into app.orgs values (1, 'Acme'), (2, 'Globex');

create table app.teams(id int primary key, org_id int not null references app.orgs, name text not null);
insert into app.teams select t, case when t <= 20 then 1 else 2 end, 'team ' || t from generate_series(1, 22) t;

create table app.org_admins(org_id int not null, user_id uuid not null, primary key (org_id, user_id));
insert into app.org_admins values (1, app.u(3)), (2, app.u(4));

-- user 1 = member (team 1 only), 2 = guest, 3 = org 1 admin, 4 = org 2 admin, 5.. = noise users
create table app.team_members(team_id int not null, user_id uuid not null, role text not null default 'member', primary key (team_id, user_id));
insert into app.team_members values (1, app.u(1), 'member');
insert into app.team_members
  select 1 + (u * 13) % 22, app.u(u), 'member' from generate_series(5, :nu) u
  on conflict do nothing;
insert into app.team_members
  select 1 + (u * 29) % 22, app.u(u), 'member' from generate_series(5, :nu) u
  on conflict do nothing;
create index on app.team_members(user_id, team_id);

-- 110 projects: team of project p is 1 + (p-1)/5
create table app.projects(id int primary key, org_id int not null, team_id int not null, name text not null);
insert into app.projects select p, case when 1 + (p - 1) / 5 <= 20 then 1 else 2 end, 1 + (p - 1) / 5, 'project ' || p
  from generate_series(1, 110) p;
create index on app.projects(team_id);
create index on app.projects(org_id);
create index on app.org_admins(user_id, org_id);

-- extra teams a project is shared with (always within the project's org); project 12 is shared with team 1
create table app.project_team_shares(project_id int not null, team_id int not null, primary key (project_id, team_id));
insert into app.project_team_shares
  select p, case when pr.org_id = 1 then 1 + (p * 7) % 20 else 21 + p % 2 end
  from generate_series(1, 110) p join app.projects pr on pr.id = p
  where p % 10 = 0 or p % 9 = 0
  on conflict do nothing;
delete from app.project_team_shares s using app.projects p where p.id = s.project_id and p.team_id = s.team_id;
insert into app.project_team_shares values (12, 1) on conflict do nothing;
create index on app.project_team_shares(team_id, project_id);

-- guests: user 2 on project 7; every 20th noise user on one project
create table app.project_guests(project_id int not null, user_id uuid not null, primary key (project_id, user_id));
insert into app.project_guests values (7, app.u(2));
insert into app.project_guests select 1 + (u * 17) % 110, app.u(u) from generate_series(5, :nu) u where u % 20 = 0 on conflict do nothing;
create index on app.project_guests(user_id, project_id);

create table app.labels(id int primary key, project_id int not null, name text not null);
insert into app.labels
  select (p - 1) * 6 + k, p, (array['bug','feature','chore','docs','design','infra'])[k] from generate_series(1, 110) p, generate_series(1, 6) k;
create index on app.labels(project_id);

-- issue i belongs to project 1 + (i-1) % 110; 2% of issues are assigned to each of users 1, 2, 3
create table app.issues(
  id int primary key, project_id int not null, title text not null, status text not null,
  assignee_id uuid, created_by uuid not null, updated_at timestamptz not null);
insert into app.issues
  select i, 1 + (i - 1) % 110,
    (array['Fix','Add','Remove','Refactor','Investigate'])[1 + i % 5] || ' ' ||
      (array['login','billing','crash','search','export','sync','deploy','webhook'])[1 + (i / 5) % 8] || ' ' ||
      (array['page','job','endpoint','widget'])[1 + (i / 40) % 4] || ' #' || i,
    (array['open','in_progress','done','done'])[1 + (i / 7) % 4],
    case when i % 50 = 0 then app.u(1) when i % 50 = 1 then app.u(2) when i % 50 = 2 then app.u(3)
         when i % 3 = 0 then null else app.u(5 + (i * 31) % (:nu - 4)) end,
    app.u(5 + (i * 17) % (:nu - 4)),
    now() - (((i::bigint * 7919) % 1000003) || ' seconds')::interval
  from generate_series(1, :ni) i;
-- indexes a real app has (see README: which query uses which)
create index issues_project_updated on app.issues(project_id, updated_at desc, id);
create index issues_project_status on app.issues(project_id, status);
create index issues_my_open on app.issues(assignee_id, updated_at desc, id) where status <> 'done';
create index issues_updated on app.issues(updated_at desc, id);

create table app.issue_labels(issue_id int not null, label_id int not null, project_id int not null, primary key (issue_id, label_id));
insert into app.issue_labels
  select i.id, (i.project_id - 1) * 6 + 1 + i.id % 6, i.project_id from app.issues i;
insert into app.issue_labels
  select i.id, (i.project_id - 1) * 6 + 1 + (i.id % 6 + 1 + (i.id / 3) % 5) % 6, i.project_id from app.issues i where i.id % 3 = 0;
create index on app.issue_labels(label_id, issue_id);
create index on app.issue_labels(project_id);

-- comments.project_id is the denormalised copy used only by the best_practice_denorm_rls variant
-- (and by the baseline); the other variants reach the project through issues
create table app.comments(
  id int primary key, issue_id int not null, project_id int not null, author_id uuid not null, body text not null, created_at timestamptz not null);
insert into app.comments
  select c, i.id, i.project_id, app.u(5 + (c * 11) % (:nu - 4)),
    (array['Looks good to me','Can you add a test','Deploy blocked by rollback','Reproduced on staging','Closing as duplicate','Needs design review'])[1 + c % 6] || ' (' || c || ')',
    now() - ((c::bigint * 104729 % 1000003) || ' seconds')::interval
  from generate_series(1, :nc) c join app.issues i on i.id = 1 + (c - 1) % :ni;
create index comments_issue on app.comments(issue_id, id);
create index comments_project on app.comments(project_id, created_at desc, id);

-- the permission, written once: used by the baseline queries and wrapped by the security definer helper
create function app.visible_projects(uid uuid) returns setof int language sql stable as $$
  select p.id from app.projects p join app.org_admins a on a.org_id = p.org_id where a.user_id = uid
  union select p.id from app.projects p join app.team_members tm on tm.team_id = p.team_id where tm.user_id = uid
  union select s.project_id from app.project_team_shares s join app.team_members tm on tm.team_id = s.team_id where tm.user_id = uid
  union select g.project_id from app.project_guests g where g.user_id = uid $$;

create function app.my_projects() returns int[] language sql stable security definer set search_path = '' as $$
  select array(select app.visible_projects((select auth.uid()))) $$;
