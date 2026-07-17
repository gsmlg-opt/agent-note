use crate::embedder::{DenseVector, Embedder, SparseVector};
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;

pub struct OrtEmbedder {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl OrtEmbedder {
    pub fn load(model_path: &Path, intra_threads: usize) -> anyhow::Result<Self> {
        // int8 quantization per docs/design.md §4 — do not attempt int4, see design.md rationale.
        let model_dir = model_path.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "model path {} has no parent directory",
                model_path.display()
            )
        })?;
        let tok_path = model_dir.join("tokenizer.json");
        let tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| anyhow::anyhow!("load tokenizer {}: {e}", tok_path.display()))?;
        // The `ort` builder methods return `Error<SessionBuilder>` (the builder is carried in the
        // error for retry), which is not `Send` and so cannot cross the `?` into `anyhow::Error`.
        // Flatten those to string-backed errors; only the final `commit_from_file` yields a plain
        // `Send` error.
        let mut builder = Session::builder()
            .map_err(|e| anyhow::anyhow!("create ort session builder: {e}"))?
            .with_intra_threads(intra_threads)
            .map_err(|e| anyhow::anyhow!("configure ort session: {e}"))?;
        let session = builder
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("load onnx model {}: {e}", model_path.display()))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }
}

#[async_trait::async_trait]
impl Embedder for OrtEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        // Inference must run via spawn_blocking, never inline on the async reactor (docs/design.md §4).
        // A single forward pass must produce both dense and sparse output (see Embedder trait doc comment).
        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let text = text.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<(DenseVector, SparseVector)> {
            let enc = tokenizer
                .encode(text, true)
                .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
            let ids: Vec<i64> = enc.get_ids().iter().map(|&x| i64::from(x)).collect();
            let mask: Vec<i64> = enc
                .get_attention_mask()
                .iter()
                .map(|&x| i64::from(x))
                .collect();
            let seq = ids.len();
            if seq == 0 {
                anyhow::bail!("empty tokenization");
            }

            let input_ids = Tensor::from_array((vec![1_i64, seq as i64], ids.clone()))?;
            let attn = Tensor::from_array((vec![1_i64, seq as i64], mask))?;
            let mut session = session.lock().unwrap();
            let outputs = session.run(ort::inputs![
                "input_ids" => input_ids,
                "attention_mask" => attn,
            ])?;

            let (_dense_shape, dense): (&_, &[f32]) =
                outputs["dense_vecs"].try_extract_tensor::<f32>()?;
            let dense_vec = dense
                .get(..1024)
                .ok_or_else(|| anyhow::anyhow!("dense_vecs output too short: {}", dense.len()))?
                .to_vec();

            let (_sparse_shape, weights): (&_, &[f32]) =
                outputs["sparse_vecs"].try_extract_tensor::<f32>()?;
            if weights.len() < seq {
                anyhow::bail!("sparse_vecs output too short: {} < {seq}", weights.len());
            }

            let mut sparse = HashMap::new();
            for i in 0..seq {
                let id = ids[i];
                if matches!(id, 0..=3) {
                    continue;
                }
                let w = weights[i];
                if w > 0.0 {
                    let e = sparse.entry(id).or_insert(0.0);
                    if w > *e {
                        *e = w;
                    }
                }
            }

            Ok((dense_vec, sparse))
        })
        .await?
    }
}
