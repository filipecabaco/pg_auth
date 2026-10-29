-- name: session_set_then_anon
select app.pa_session_set_then_anon()

-- name: set_local_then_anon
select app.pa_set_local_then_anon()

-- name: set_local_ok
select app.pa_set_local_ok()

-- name: local_outside_tx
select app.pa_local_outside_tx()

-- name: fresh_anon
select app.pa_fresh_anon()

-- name: spoof_org2
select app.pa_spoof('2')
