use crate::embedder::{DenseVector, Embedder, SparseVector};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Wraps an `Embedder`, bounding concurrent `embed()` calls with a semaphore so queuing is
/// visible at the app layer rather than absorbed as creeping tail latency (docs/design.md §4).
pub struct BoundedEmbedder<E: Embedder + 'static> {
    inner: Arc<E>,
    semaphore: Arc<Semaphore>,
}

impl<E: Embedder + 'static> BoundedEmbedder<E> {
    pub fn new(inner: E, max_concurrent: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }
}

#[async_trait::async_trait]
impl<E: Embedder + 'static> Embedder for BoundedEmbedder<E> {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        let _permit = self.semaphore.acquire().await?;
        self.inner.embed(text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct CountingEmbedder {
        in_flight: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Embedder for CountingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_observed.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok((vec![0.0; 1024], Default::default()))
        }
    }

    #[tokio::test]
    async fn limits_concurrent_calls_to_semaphore_size() {
        let max_observed = Arc::new(AtomicUsize::new(0));
        let inner = CountingEmbedder {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_observed: max_observed.clone(),
        };
        let bounded = Arc::new(BoundedEmbedder::new(inner, 2));

        let mut handles = vec![];
        for _ in 0..10 {
            let b = bounded.clone();
            handles.push(tokio::spawn(async move { b.embed("x").await.unwrap() }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert!(
            max_observed.load(Ordering::SeqCst) <= 2,
            "concurrency exceeded semaphore limit"
        );
    }
}
