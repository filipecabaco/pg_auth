-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth enforcing Cerbos's rules on the rows' attributes (pg_auth/cerbos): each plan compiled
-- into a filter on tenant_id (owner_id for notes).
select authz.configure(subject => 'jwt', claims_setting => 'request.jwt.claims', claim => 'sub');
select authz_internal.protect('app.events', 'tenant_row', 'id', 'read', attributes => array['tenant_id']);
select authz_internal.protect('app.events_p', 'tenant_row', 'id', 'read', attributes => array['tenant_id']);
select authz_internal.protect('app.projects', 'tenant_row', 'id', 'read', attributes => array['tenant_id']);
select authz_internal.protect('app.notes', 'note', 'id', 'read', attributes => array['owner_id']);
