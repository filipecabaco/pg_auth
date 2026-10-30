-- Cedar's parents (child, parent): each membership puts the org in the member's scope, and each
-- document is in its org
select 'Org:' || org_id, 'User:' || user_id from app.memberships
union all
select 'Document:' || id, 'Org:' || org_id from app.documents
