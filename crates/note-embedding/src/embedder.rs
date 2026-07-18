pub type DenseVector = Vec<f32>; // 1024-d, L2-normalized

pub const DEFAULT_BGE_M3_MODEL: &str = "bge-m3";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingBackendInfo {
    pub engine: String,
    pub model: String,
    pub fingerprint: String,
}

pub fn embedding_fingerprint(model: &str) -> String {
    format!(
        "{}:{}",
        model.trim(),
        crate::rpc::DEFAULT_EMBEDDING_DIMENSION
    )
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
    async fn embed(&self, text: &str) -> anyhow::Result<DenseVector>;

    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<DenseVector>> {
        let mut outputs = Vec::with_capacity(texts.len());
        for text in texts {
            outputs.push(self.embed(text).await?);
        }
        Ok(outputs)
    }
}
