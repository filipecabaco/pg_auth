-- non-leakproof: ~~ (LIKE) on a trigram GIN index, rare pattern
-- name: like_rare
select count(*) from app.documents where body like '%' || substr(md5('4242'), 3, 8) || '%'

-- non-leakproof: ~~ with a 3-char pattern (~1.5% of rows)
-- name: like_common
select count(*) from app.documents where body like '%abc%'

-- non-leakproof: jsonb @> on a GIN jsonb_path_ops index
-- name: jsonb_contains
select count(*) from app.documents where meta @> '{"status":"draft","n":7}'

-- non-leakproof: lower() expression index
-- name: lower_eq
select count(*) from app.documents where lower(title) = 'doc-77'

-- non-leakproof: numeric_eq on a btree index
-- name: numeric_eq
select count(*) from app.documents where price = 7.5

-- control, leakproof: texteq on a btree index
-- name: text_eq
select count(*) from app.documents where title = 'Doc-77'

-- control, leakproof: int8 range on the pk
-- name: id_range
select count(*) from app.documents where id between 1000 and 1999
