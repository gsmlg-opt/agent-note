use crate::embedder::{DenseVector, Embedder, SparseVector};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub struct StubEmbedder;

#[async_trait::async_trait]
impl Embedder for StubEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
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

        // A handful of deterministic "token" entries derived from word hashes.
        let mut sparse = SparseVector::new();
        for word in text.split_whitespace() {
            let mut wh = DefaultHasher::new();
            word.hash(&mut wh);
            let token_id = (wh.finish() % 100_000) as i64;
            sparse.insert(token_id, 1.0);
        }
        if sparse.is_empty() {
            sparse.insert(0, 1.0); // never return a fully empty sparse map
        }

        Ok((dense, sparse))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dense_vector_is_1024_dimensional() {
        let (dense, _) = StubEmbedder.embed("hello world").await.unwrap();
        assert_eq!(dense.len(), 1024);
    }

    #[tokio::test]
    async fn same_input_produces_same_output() {
        let (d1, s1) = StubEmbedder.embed("hello world").await.unwrap();
        let (d2, s2) = StubEmbedder.embed("hello world").await.unwrap();
        assert_eq!(d1, d2);
        assert_eq!(s1, s2);
    }

    #[tokio::test]
    async fn different_input_produces_different_output() {
        let (d1, _) = StubEmbedder.embed("hello").await.unwrap();
        let (d2, _) = StubEmbedder.embed("world").await.unwrap();
        assert_ne!(d1, d2);
    }

    #[tokio::test]
    async fn sparse_vector_is_nonempty() {
        let (_, sparse) = StubEmbedder.embed("hello world").await.unwrap();
        assert!(!sparse.is_empty());
    }
}
