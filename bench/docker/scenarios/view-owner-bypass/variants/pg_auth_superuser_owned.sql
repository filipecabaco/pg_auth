-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- Views and the definer function owned by a superuser (the postgres role that ran the migration): superusers
-- always skip RLS, FORCE or not.
alter view app.docs_v set (security_invoker = false);
alter view app.docs_vb set (security_invoker = false);
alter view app.docs_f set (security_invoker = false);
alter function app.docs_fn() security definer;
alter view app.docs_v owner to postgres;
alter view app.docs_vb owner to postgres;
alter view app.docs_f owner to postgres;
alter function app.docs_fn() owner to postgres;
select authz_internal.protect('app.docs', 'org', 'org_id', 'member');
