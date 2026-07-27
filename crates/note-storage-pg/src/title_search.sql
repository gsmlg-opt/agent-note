WITH query AS (
    SELECT to_tsquery('simple'::regconfig, $1) AS terms
)
SELECT notes.id
FROM notes
CROSS JOIN query
WHERE notes.deleted_at IS NULL
  AND ($2::text[] IS NULL OR notes.id = ANY($2))
  AND notes.title_fts @@ query.terms
ORDER BY ts_rank_cd(notes.title_fts, query.terms) DESC, notes.id ASC
LIMIT $3
