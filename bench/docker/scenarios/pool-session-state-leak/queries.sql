-- name: session_set_then_anon
select app.session_set_then_anon()

-- name: set_local_then_anon
select app.set_local_then_anon()

-- name: set_local_ok
select app.set_local_ok()

-- name: local_outside_tx
select app.local_outside_tx()

-- name: fresh_anon
select app.fresh_anon()

-- name: spoof_org2
select app.spoof('2')
