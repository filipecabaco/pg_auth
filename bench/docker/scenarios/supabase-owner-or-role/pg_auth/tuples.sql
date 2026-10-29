-- Per owner, not per todo: the account's own user, and the app it belongs to. Role holders on the app.
select distinct 'account:' || owner_id, 'holder', 'user:' || owner_id from app.todos
union all
select distinct 'account:' || owner_id, 'app', 'app:main' from app.todos
union all
select 'app:main', role, 'user:' || user_id from app.user_roles
