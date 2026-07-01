use libsql::Connection;

// Caller's responsibility: embedding components must be finite (real embedders emit normalized,
// finite values). NaN/Infinity aren't valid JSON number tokens and would produce a malformed
// vector literal here rather than a clean error.
fn vector_to_json(v: &[f32]) -> String {
    let elems: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    format!("[{}]", elems.join(","))
}

pub async fn insert_embedding(
    conn: &Connection,
    note_id: &str,
    embedding: &[f32],
) -> anyhow::Result<()> {
    let json = vector_to_json(embedding);
    conn.execute(
        "INSERT INTO notes_embeddings (note_id, embedding) VALUES (?1, vector(?2))",
        libsql::params![note_id, json],
    )
    .await?;
    Ok(())
}

pub async fn dense_ann_query(
    conn: &Connection,
    query: &[f32],
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let json = vector_to_json(query);
    // vector_top_k() returns rows from the vector index's shadow table, whose `id` column is the
    // rowid of the indexed table (notes_embeddings) — not notes_embeddings' declared `note_id`
    // TEXT PRIMARY KEY. The JOIN recovers note_id via that rowid; it isn't optional/simplifiable.
    let sql = "SELECT e.note_id FROM vector_top_k('idx_notes_embedding', ?1, ?2) AS t
               JOIN notes_embeddings e ON e.rowid = t.id";
    let mut rows = conn
        .query(sql, libsql::params![json, limit as i64])
        .await?;
    let mut ids = vec![];
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}

pub async fn insert_sparse_weights(
    conn: &Connection,
    note_id: &str,
    weights: &[(i64, f64)],
) -> anyhow::Result<()> {
    for (token_id, weight) in weights {
        conn.execute(
            "INSERT INTO notes_sparse_weights (note_id, token_id, weight) VALUES (?1, ?2, ?3)",
            libsql::params![note_id, *token_id, *weight],
        )
        .await?;
    }
    Ok(())
}

pub async fn sparse_postings_query(
    conn: &Connection,
    token_ids: &[i64],
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let placeholders: Vec<String> = (1..=token_ids.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT note_id, SUM(weight) as total FROM notes_sparse_weights
         WHERE token_id IN ({}) GROUP BY note_id ORDER BY total DESC LIMIT {}",
        placeholders.join(","),
        limit
    );
    let params: Vec<libsql::Value> = token_ids.iter().map(|t| (*t).into()).collect();
    let mut rows = conn.query(&sql, params).await?;
    let mut ids = vec![];
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}
