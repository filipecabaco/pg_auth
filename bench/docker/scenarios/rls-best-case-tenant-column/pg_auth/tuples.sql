-- tenant membership, and one account object per user for the owner_id = auth.uid() pattern
select 'tenant:' || tenant_id, 'member', 'user:' || user_id from app.members
union all
select distinct 'account:' || user_id, 'owner', 'user:' || user_id from app.members
