pub type DenseVector = Vec<f32>;

pub const DEFAULT_BGE_M3_MODEL: &str = "bge-m3";

pub fn embedding_dimension(model: &str) -> usize {
    if is_embeddinggemma2(model) {
        768
    } else {
        crate::rpc::DEFAULT_EMBEDDING_DIMENSION
    }
}

pub(crate) fn is_embeddinggemma2(model: &str) -> bool {
    matches!(model.trim(), "embeddinggemma-2" | "google/embeddinggemma-2")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingBackendInfo {
    pub engine: String,
    pub model: String,
    pub fingerprint: String,
}

pub fn embedding_fingerprint(model: &str) -> String {
    format!("{}:{}", model.trim(), embedding_dimension(model))
}

impl EmbeddingBackendInfo {
    pub fn local_stub() -> Self {
        Self {
            engine: "local".into(),
            model: "stub".into(),
            fingerprint: embedding_fingerprint("stub"),
        }
    }

    pub fn local_bge_m3() -> Self {
        Self {
            engine: "local".into(),
            model: DEFAULT_BGE_M3_MODEL.into(),
            fingerprint: embedding_fingerprint(DEFAULT_BGE_M3_MODEL),
        }
    }

    pub fn openai(model: impl Into<String>) -> Self {
        let model = model.into().trim().to_owned();
        Self {
            engine: "openai".into(),
            fingerprint: embedding_fingerprint(&model),
            model,
        }
    }
}

#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// Encode a document. Model-specific retrieval instructions belong in the adapter.
    async fn embed(&self, text: &str) -> anyhow::Result<DenseVector>;

    async fn embed_query(&self, text: &str) -> anyhow::Result<DenseVector> {
        self.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<DenseVector>> {
        let mut outputs = Vec::with_capacity(texts.len());
        for text in texts {
            outputs.push(self.embed(text).await?);
        }
        Ok(outputs)
    }
}
