-- name: list
select id, title from app.todos order by id

-- name: page
select id from app.todos order by created_at desc, id desc limit 20

-- name: count
select count(*) from app.todos

-- name: join
select t.id, i.id as item_id from app.todos t join app.todo_items i on i.todo_id = t.id order by t.created_at desc, i.id limit 20

-- name: items
select count(*) from app.todo_items

-- name: pk_a
select * from app.todos where id = 1

-- name: pk_b
select * from app.todos where id = 1000

-- name: open
select id from app.todos where not done order by created_at desc, id desc limit 20
