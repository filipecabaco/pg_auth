-- name: same_flat
select app.loop_flat(500, false)

-- name: alt_flat
select app.loop_flat(500, true)

-- name: same_part
select app.loop_part(500, false)

-- name: alt_part
select app.loop_part(500, true)
