use crate::embedder::{DenseVector, Embedder};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub struct StubEmbedder;

#[async_trait::async_trait]
impl Embedder for StubEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<DenseVector> {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let seed = hasher.finish();

        // Deterministic pseudo-random dense vector, seeded from the text hash.
        let mut dense = Vec::with_capacity(1024);
        let mut state = seed;
        for _ in 0..1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            dense.push(((state >> 33) as f32 / u32::MAX as f32) - 0.5);
        }
        // Not reachable with this LCG (would require hitting the exact 31-bit midpoint on all
        // 1024 iterations), but a real model (Task 12's OrtEmbedder) could plausibly emit an
        // all-zero vector for degenerate input, so don't copy this normalization unguarded there.
        let norm: f32 = dense.iter().map(|x| x * x).sum::<f32>().sqrt();
        for v in dense.iter_mut() {
            *v /= norm;
        }

        Ok(dense)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dense_vector_is_1024_dimensional() {
        let dense = StubEmbedder.embed("hello world").await.unwrap();
        assert_eq!(dense.len(), 1024);
    }

    #[tokio::test]
    async fn same_input_produces_same_direct_vector() {
        let d1 = StubEmbedder.embed("hello world").await.unwrap();
        let d2 = StubEmbedder.embed("hello world").await.unwrap();
        assert_eq!(d1, d2);
    }

    #[tokio::test]
    async fn different_input_produces_different_direct_vectors() {
        let d1 = StubEmbedder.embed("hello").await.unwrap();
        let d2 = StubEmbedder.embed("world").await.unwrap();
        assert_ne!(d1, d2);
    }

    #[tokio::test]
    async fn batch_returns_one_dense_vector_per_input() {
        let inputs = vec!["hello".to_string(), "world".to_string()];
        let outputs = StubEmbedder.embed_batch(&inputs).await.unwrap();
        assert_eq!(outputs.len(), 2);
        assert!(outputs.iter().all(|dense| dense.len() == 1024));
    }
}
