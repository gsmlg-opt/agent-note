use crate::embedder::{DenseVector, Embedder, SparseVector};
use std::path::Path;

pub struct OrtEmbedder {
    // session: ort::session::Session, // populate once the current `ort` API is confirmed
}

impl OrtEmbedder {
    pub fn load(_model_path: &Path) -> anyhow::Result<Self> {
        // TODO: build an ort::Session from model_path once the exact ort API (session builder,
        // execution providers, int8 handling) is confirmed against current docs.rs/ort.
        // int8 quantization per docs/design.md §4 — do not attempt int4, see design.md rationale.
        anyhow::bail!("OrtEmbedder::load not yet implemented — see Task 12 in the implementation plan")
    }
}

#[async_trait::async_trait]
impl Embedder for OrtEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        // Inference must run via spawn_blocking, never inline on the async reactor (docs/design.md §4).
        // A single forward pass must produce both dense and sparse output (see Embedder trait doc comment).
        anyhow::bail!("OrtEmbedder::embed not yet implemented — see Task 12 in the implementation plan")
    }
}
