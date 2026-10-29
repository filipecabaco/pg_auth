-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth production defaults with org_id indexed: what the missing index costs pg_auth.
create index if not exists documents_org_idx on app.documents(org_id);
select authz.protect('app.documents', 'org', 'org_id', 'member');
