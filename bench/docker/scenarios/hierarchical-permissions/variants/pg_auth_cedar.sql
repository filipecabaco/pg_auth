-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth enforcing Cedar, run by the sidecar (pg_auth/cedar): the folders under a user's grants,
-- listed when they are few, else each file's folder checked by Cedar with its parents.
select authz.configure(subject_type => 'User');
\set schema `cat /scenario/pg_auth/cedar/schema.cedarschema`
\set policies `cat /scenario/pg_auth/cedar/policies.cedar`
select authz.set_rules('cedar', 'schema', :'schema');
select authz.set_rules('cedar', 'policies', :'policies');
select authz_internal.protect('app.files', 'Folder', 'folder_id', 'read');
