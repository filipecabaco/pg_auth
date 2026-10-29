-- name: list
select id, title from app.todos where (owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support')) order by id

-- name: page
select id from app.todos where (owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support')) order by created_at desc, id desc limit 20

-- name: count
select count(*) from app.todos where (owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'))

-- name: join
select t.id, i.id as item_id from app.todos t join app.todo_items i on i.todo_id = t.id where (t.owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support')) order by t.created_at desc, i.id limit 20

-- name: items
select count(*) from app.todo_items where (owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'))

-- name: pk_a
select * from app.todos where id = 1 and (owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'))

-- name: pk_b
select * from app.todos where id = 1000 and (owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support'))

-- name: open
select id from app.todos where not done and (owner_id = '{{subject}}'::uuid or (select auth.jwt() -> 'app_metadata' ->> 'role') in ('admin', 'support')) order by created_at desc, id desc limit 20
