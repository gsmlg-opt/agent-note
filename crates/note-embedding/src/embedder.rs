use std::collections::HashMap;

pub type DenseVector = Vec<f32>; // 1024-d, L2-normalized
pub type SparseVector = HashMap<i64, f32>; // token_id -> weight, thresholded

/// A single dense+sparse inference call. Do not add a second method to this trait that splits
/// dense/sparse apart — BGE-M3 produces both from one forward pass (docs/design.md §4/§6).
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// Dense + sparse output from a single inference call (docs/design.md §4/§6 — must not be
    /// split into two separate calls/methods, since BGE-M3 produces both from one forward pass).
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)>;

    async fn embed_batch(
        &self,
        texts: &[String],
    ) -> anyhow::Result<Vec<(DenseVector, SparseVector)>> {
        let mut outputs = Vec::with_capacity(texts.len());
        for text in texts {
            outputs.push(self.embed(text).await?);
        }
        Ok(outputs)
    }
}
