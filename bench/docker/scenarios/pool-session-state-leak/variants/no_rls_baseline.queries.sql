-- Correct answers: anonymous / spoofing requests see nothing; the legitimate request sees its org.
-- name: session_set_then_anon
select 0::bigint

-- name: set_local_then_anon
select 0::bigint

-- name: set_local_ok
select count(*) from app.docs where org_id = {{subject}}

-- name: local_outside_tx
select 0::bigint

-- name: fresh_anon
select 0::bigint

-- name: spoof_org2
select 0::bigint
