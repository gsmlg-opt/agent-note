use crate::{chunk_content, Context};

pub async fn backfill_chunk_embeddings(ctx: &Context) -> anyhow::Result<usize> {
    let conn = ctx.storage.connect()?;
    let mut rows = conn
        .query(
            "SELECT id, content FROM notes WHERE id NOT IN (SELECT DISTINCT note_id FROM note_chunk_embeddings)",
            (),
        )
        .await?;
    let mut notes = Vec::new();
    while let Some(row) = rows.next().await? {
        notes.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    drop(rows);

    let mut processed = 0;
    for (id, content) in notes {
        let chunks = chunk_content(&content);
        let mut embeddings = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            let (dense, sparse) = ctx.embedder.embed(chunk).await?;
            let weights: Vec<(i64, f64)> = sparse.into_iter().map(|(k, v)| (k, v as f64)).collect();
            embeddings.push((dense, weights));
        }

        let tx = conn.transaction().await?;
        for (idx, (dense, weights)) in embeddings.iter().enumerate() {
            note_storage::insert_chunk_embedding(&tx, &id, idx as i64, dense).await?;
            note_storage::insert_chunk_sparse_weights(&tx, &id, idx as i64, weights).await?;
        }
        tx.commit().await?;
        processed += 1;
    }

    Ok(processed)
}
