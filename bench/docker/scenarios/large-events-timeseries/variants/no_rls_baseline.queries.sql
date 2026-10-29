-- name: last_hour
select count(*) from app.events where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and created_at >= timestamptz '2026-10-01' - interval '1 hour'

-- name: day_counts
select kind, count(*) from app.events where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and created_at >= timestamptz '2026-10-01' - interval '1 day' group by kind order by kind

-- name: payload_filter
select count(*) from app.events where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and payload @> '{"plan": "enterprise"}'

-- name: range_all
select tenant_id, count(*) c from app.events where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and created_at >= timestamptz '2026-09-28' and created_at < timestamptz '2026-09-28 03:00' group by tenant_id order by c desc, tenant_id limit 10

-- name: latest_n
select id from app.events where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) order by created_at desc limit 50

-- name: hot_kind
select id from app.events where tenant_id in (select m.tenant_id from app.memberships m where m.user_id = {{subject}}) and kind = 'error' order by created_at desc limit 20
