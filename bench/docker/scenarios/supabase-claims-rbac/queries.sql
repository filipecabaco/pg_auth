-- name: channels
select id, name from app.channels order by id

-- name: recent
select id from app.messages order by created_at desc, id desc limit 20

-- name: count
select count(*) from app.messages

-- name: join
select count(*) from app.messages m join app.channels c on c.id = m.channel_id

-- name: join_page
select m.id, c.name from app.messages m join app.channels c on c.id = m.channel_id order by m.created_at desc, m.id desc limit 20

-- name: feed_a
select id from app.messages where channel_id = 4 order by created_at desc, id desc limit 20

-- name: feed_b
select id from app.messages where channel_id = 62 order by created_at desc, id desc limit 20

-- name: by_org
select count(*) from app.messages where org_id = 63

-- name: pk
select * from app.messages where id = 4242
