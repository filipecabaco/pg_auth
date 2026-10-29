-- Each owner is a member of their own account; documents.owner_id names the account.
select distinct 'account:' || owner_id, 'member', 'user:' || owner_id from app.documents
