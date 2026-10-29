-- name: latest50
select id, user_id, created_at from app.messages where room_id = 1 order by created_at desc, id desc limit 50

-- name: unread
select m.room_id, count(*) as unread
from app.room_reads rr join app.messages m on m.room_id = rr.room_id and m.created_at > rr.last_read_at
where rr.user_id = auth.uid()
group by m.room_id order by m.room_id

-- name: search
select id from app.messages where body like '%777%' order by id limit 20

-- name: members
select user_id, role from app.room_members where room_id = 3 order by user_id

-- name: my_rooms_last
select r.id, lm.id as last_id, lm.created_at
from app.rooms r
join app.room_members rm on rm.room_id = r.id and rm.user_id = auth.uid()
cross join lateral (select m.id, m.created_at from app.messages m where m.room_id = r.id
                    order by m.created_at desc, m.id desc limit 1) lm
order by r.id

-- name: reactions
select emoji, count(*) from app.reactions where room_id = 1 group by emoji order by emoji

-- name: count
select count(*) from app.messages
