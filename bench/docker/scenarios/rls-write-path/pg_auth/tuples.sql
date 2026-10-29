-- roles per tenant, and (for the pg_auth_row_object variant) one tuple per draft: the row's parent tenant
select 'tenant:' || tenant_id, role, 'user:' || user_id from app.members
union all
select 'draft:' || id, 'tenant', 'tenant:' || tenant_id from app.drafts
