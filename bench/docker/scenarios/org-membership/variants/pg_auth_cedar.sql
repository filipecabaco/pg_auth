-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth enforcing Cedar, run by the sidecar (pg_auth/cedar): documents by their org, comments by
-- their document, each an `in` the member's scope.
select authz.configure(subject_type => 'User');
\set schema `cat /scenario/pg_auth/cedar/schema.cedarschema`
\set policies `cat /scenario/pg_auth/cedar/policies.cedar`
select authz.set_rules('cedar', 'schema', :'schema');
select authz.set_rules('cedar', 'policies', :'policies');
select authz_internal.protect('app.documents', 'Org', 'org_id', 'read');
select authz_internal.protect('app.documents_p', 'Org', 'org_id', 'read');
select authz_internal.protect('app.comments', 'Document', 'document_id', 'read');
