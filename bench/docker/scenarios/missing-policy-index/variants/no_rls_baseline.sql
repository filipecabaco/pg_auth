-- @role: superuser
-- No RLS, semi-join in the SQL, index on org_id present (the target).
create index if not exists documents_org_idx on app.documents(org_id);
