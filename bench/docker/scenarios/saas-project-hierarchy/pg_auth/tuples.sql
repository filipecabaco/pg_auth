-- The access facts the RLS variants read from app.* tables, as OpenFGA tuples. Org admin -> team ->
-- project is inheritance in the model. One tuple per membership, share or guest, never per issue or comment.
select 'org:' || org_id, 'admin', 'user:' || user_id from app.org_admins
union all select 'team:' || id, 'org', 'org:' || org_id from app.teams
union all select 'team:' || team_id, 'member', 'user:' || user_id from app.team_members
union all select 'project:' || id, 'owner_team', 'team:' || team_id from app.projects
union all select 'project:' || project_id, 'shared_team', 'team:' || team_id from app.project_team_shares
union all select 'project:' || project_id, 'guest', 'user:' || user_id from app.project_guests
