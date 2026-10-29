-- name: fts_rare
select count(*) from app.documents where tsv @@ to_tsquery('simple', 'term99990')

-- name: fts_mid
select count(*) from app.documents where tsv @@ to_tsquery('simple', 'term5000')

-- name: fts_common
select count(*) from app.documents where tsv @@ to_tsquery('simple', 'term0')

-- name: ilike_rare
select count(*) from app.documents where title ilike '%term8472%'

-- name: ilike_common
select count(*) from app.documents where title ilike '%term1%'

-- name: rank_top20
select id from app.documents where tsv @@ to_tsquery('simple', 'term50') order by ts_rank(tsv, to_tsquery('simple', 'term50')) desc, id limit 20
