-- The per-document grants the RLS variants read, as OpenFGA tuples.
select 'document:' || doc_id, 'viewer', 'user:' || user_id from app.doc_grants
