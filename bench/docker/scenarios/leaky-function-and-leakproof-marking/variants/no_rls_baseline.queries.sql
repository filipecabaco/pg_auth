-- name: where_clause
select s.n, app.leaked() as leaked from (select count(*) as n from (select * from app.docs where org_id = {{subject}} offset 0) d where app.spy(org_id)) s

-- name: join_on
select s.n, app.leaked() as leaked from (
  select count(*) as n from (select * from app.docs where org_id = {{subject}} offset 0) d join app.orgs o on o.id = d.org_id and app.spy(d.org_id)) s

-- name: subq_tlist
select s.n, app.leaked() as leaked from (select count(*) as n from (select app.spy_i(org_id) as x from (select * from app.docs where org_id = {{subject}} offset 0) d) t) s

-- name: cte
select s.n, app.leaked() as leaked from (
  select count(*) as n from (with c as materialized (select id, org_id from (select * from app.docs where org_id = {{subject}} offset 0) d where app.spy(org_id)) select * from c) c2) s

-- name: order_by
select s.n, app.leaked() as leaked from (
  select count(*) as n from (select id from (select * from app.docs where org_id = {{subject}} offset 0) d order by app.spy_i(org_id), id limit 1000000) t) s

-- name: view
select s.n, app.leaked() as leaked from (select count(*) as n from (select * from app.docs where org_id = {{subject}} offset 0) d where app.spy(org_id)) s

-- name: tlist_top
select count(*) as n, (select app.leaked()) as leaked from (select app.spy_i(org_id) from (select * from app.docs where org_id = {{subject}} offset 0) d) t

-- name: cast_error
select count(*) from (select * from app.docs where org_id = {{subject}} offset 0) d where secret::int > 0

-- name: udf_error
select count(*) from (select * from app.docs where org_id = {{subject}} offset 0) d where app.to_int(secret) > 0

-- name: agg_filter
select s.c, app.leaked() as leaked from (select count(*) filter (where app.spy(org_id)) as c from (select * from app.docs where org_id = {{subject}} offset 0) d) s

-- name: order_limit
select s.c, app.leaked() as leaked from (select count(*) as c from (select * from (select * from app.docs where org_id = {{subject}} offset 0) d order by app.spy_i(org_id) limit 5) x) s

-- name: group_by
select s.c, app.leaked() as leaked from (select count(*) as c from (select app.spy_i(org_id) g, count(*) from (select * from app.docs where org_id = {{subject}} offset 0) d group by 1) x) s

-- name: lateral
select s.c, app.leaked() as leaked from (select count(*) as c from (select * from app.docs where org_id = {{subject}} offset 0) d, lateral (select app.spy_i(d.org_id) as x) l) s

-- name: subq_in
select s.c, app.leaked() as leaked from (select count(*) as c from (select * from app.docs where org_id = {{subject}} offset 0) d where id in (select id from (select * from app.docs where org_id = {{subject}} offset 0) d2 where app.spy(d2.org_id))) s
