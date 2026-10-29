-- Tenant-level facts only: the tuple count is the membership count, however many rows the data table has.
select 'tenant:' || tenant_id, 'member', 'user:' || user_id from app.memberships
