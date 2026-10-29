-- name: count_all
select count(*) from app.events

-- name: kind_recent
select * from app.events where kind = 7 order by ts desc limit 50

-- name: kind_count
select count(*) from app.events where kind = 7

-- name: join_devices
select d.model, count(*) from app.events e join app.devices d on d.id = e.device_id where e.kind = 7 group by d.model order by 2 desc, 1 limit 5

-- name: latest
select * from app.events order by ts desc limit 50
