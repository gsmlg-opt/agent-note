pub type DenseVector = Vec<f32>; // 1024-d, L2-normalized

#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> anyhow::Result<DenseVector>;

    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<DenseVector>> {
        let mut outputs = Vec::with_capacity(texts.len());
        for text in texts {
            outputs.push(self.embed(text).await?);
        }
        Ok(outputs)
    }
}
