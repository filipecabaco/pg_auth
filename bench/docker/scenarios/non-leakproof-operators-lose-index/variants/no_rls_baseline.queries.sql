-- name: like_rare
select count(*) from app.documents where body like '%' || substr(md5('4242'), 3, 8) || '%' and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: like_common
select count(*) from app.documents where body like '%abc%' and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: jsonb_contains
select count(*) from app.documents where meta @> '{"status":"draft","n":7}' and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: lower_eq
select count(*) from app.documents where lower(title) = 'doc-77' and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: numeric_eq
select count(*) from app.documents where price = 7.5 and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: text_eq
select count(*) from app.documents where title = 'Doc-77' and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})

-- name: id_range
select count(*) from app.documents where id between 1000 and 1999 and org_id in (select m.org_id from app.memberships m where m.user_id = {{subject}})
