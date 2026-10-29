-- name: same_flat
select count(*) from app.documents where id in (select (i::bigint * 7919) % (select count(*) from app.documents) + 1 from generate_series(1, 500) i) and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: alt_flat
select count(*) from app.documents where id in (select (i::bigint * 7919) % (select count(*) from app.documents) + 1 from generate_series(1, 500) i) and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: same_part
select count(*) from app.documents_p where id in (select (i::bigint * 7919) % (select count(*) from app.documents) + 1 from generate_series(1, 500) i) and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: alt_part
select count(*) from app.documents_p where id in (select (i::bigint * 7919) % (select count(*) from app.documents) + 1 from generate_series(1, 500) i) and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})
