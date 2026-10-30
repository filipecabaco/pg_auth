-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- force => false: the table owner (app_owner) skips the policy, so views and the definer function it owns leak.
alter view app.docs_v set (security_invoker = false);
alter view app.docs_vb set (security_invoker = false);
alter view app.docs_f set (security_invoker = false);
alter function app.docs_fn() security definer;
alter view app.docs_v owner to app_owner;
alter view app.docs_vb owner to app_owner;
alter view app.docs_f owner to app_owner;
alter function app.docs_fn() owner to app_owner;
select authz_internal.protect('app.docs', 'org', 'org_id', 'member', null, false);
