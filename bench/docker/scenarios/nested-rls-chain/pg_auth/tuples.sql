-- The memberships plus the document -> comment chain the RLS variants walk, as OpenFGA tuples.
select 'org:' || org_id, 'member', 'user:' || user_id from app.memberships
union all
select 'document:' || id, 'org', 'org:' || org_id from app.documents
union all
select 'comment:' || id, 'document', 'document:' || document_id from app.comments
