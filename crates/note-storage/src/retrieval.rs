use libsql::Connection;
use std::collections::HashSet;

// Caller's responsibility: embedding components must be finite (real embedders emit normalized,
// finite values). NaN/Infinity aren't valid JSON number tokens and would produce a malformed
// vector literal here rather than a clean error.
fn vector_to_json(v: &[f32]) -> String {
    let elems: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    format!("[{}]", elems.join(","))
}

pub async fn insert_chunk_embedding(
    conn: &Connection,
    note_id: &str,
    chunk_idx: i64,
    embedding: &[f32],
) -> anyhow::Result<()> {
    let json = vector_to_json(embedding);
    conn.execute(
        "INSERT INTO note_chunk_embeddings (note_id, chunk_idx, embedding) VALUES (?1, ?2, vector(?3))",
        libsql::params![note_id, chunk_idx, json],
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
    // rowid of the indexed table (note_chunk_embeddings), not its declared note/chunk key. The
    // JOIN recovers note_id via that rowid; it isn't optional/simplifiable.
    let sql = "SELECT e.note_id FROM vector_top_k('idx_note_chunk_embedding', ?1, ?2) AS t
               JOIN note_chunk_embeddings e ON e.rowid = t.id";
    let mut rows = conn
        .query(sql, libsql::params![json, (limit * 8) as i64])
        .await?;
    let mut seen = HashSet::new();
    let mut ids = vec![];
    while let Some(row) = rows.next().await? {
        let id = row.get::<String>(0)?;
        if seen.insert(id.clone()) {
            ids.push(id);
            if ids.len() >= limit {
                break;
            }
        }
    }
    Ok(ids)
}

pub async fn insert_chunk_sparse_weights(
    conn: &Connection,
    note_id: &str,
    chunk_idx: i64,
    weights: &[(i64, f64)],
) -> anyhow::Result<()> {
    for (token_id, weight) in weights {
        conn.execute(
            "INSERT INTO note_chunk_sparse (note_id, chunk_idx, token_id, weight) VALUES (?1, ?2, ?3, ?4)",
            libsql::params![note_id, chunk_idx, *token_id, *weight],
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
        "SELECT note_id, MAX(chunk_total) AS total FROM (
             SELECT note_id, chunk_idx, SUM(weight) AS chunk_total
             FROM note_chunk_sparse
             WHERE token_id IN ({})
             GROUP BY note_id, chunk_idx
         ) GROUP BY note_id ORDER BY total DESC LIMIT {}",
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
