use note_embedding::{BoundedEmbedder, StubEmbedder};
use note_pipelines::{Context, ProcessEmbeddingJobStatus};
use note_storage::Storage;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_EMBEDDING_WORKERS: usize = 1;
const DEFAULT_POLL_MS: u64 = 1000;

fn build_embedder(concurrency: usize) -> anyhow::Result<Arc<dyn note_embedding::Embedder>> {
    match std::env::var("NOTE_MODEL_PATH") {
        Ok(p) if !p.is_empty() => {
            eprintln!("embedder: ONNX ({p})");
            let ort = note_embedding::OrtEmbedder::load(std::path::Path::new(&p))?;
            Ok(Arc::new(BoundedEmbedder::new(ort, concurrency)))
        }
        _ => {
            eprintln!("embedder: stub");
            Ok(Arc::new(BoundedEmbedder::new(StubEmbedder, concurrency)))
        }
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = std::env::var("NOTE_DB_PATH").unwrap_or_else(|_| "notes.db".to_string());
    let workers = env_usize("NOTE_EMBEDDING_WORKERS", DEFAULT_EMBEDDING_WORKERS);
    let poll_interval = Duration::from_millis(env_u64("NOTE_EMBEDDING_POLL_MS", DEFAULT_POLL_MS));

    let storage = Storage::open_local(&db_path).await?;
    let embedder = build_embedder(workers)?;
    let ctx = Arc::new(Context::new(Arc::new(storage), embedder));

    let requeued = note_pipelines::requeue_processing_embedding_jobs(&ctx).await?;
    let queued = note_pipelines::enqueue_missing_chunk_embeddings(&ctx).await?;
    eprintln!("embedding-service started: workers={workers}, queued={queued}, requeued={requeued}");

    loop {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                note_pipelines::process_next_embedding_job(&ctx).await
            }));
        }

        let mut processed = 0;
        for handle in handles {
            match handle.await? {
                Ok(Some(job)) => {
                    processed += 1;
                    match job.status {
                        ProcessEmbeddingJobStatus::Completed => {
                            eprintln!("embedded note {} chunk {}", job.note_id, job.chunk_idx)
                        }
                        ProcessEmbeddingJobStatus::Stale => eprintln!(
                            "skipped stale embedding job {} for note {} chunk {}",
                            job.job_id, job.note_id, job.chunk_idx
                        ),
                        ProcessEmbeddingJobStatus::Failed => eprintln!(
                            "embedding job {} failed for note {} chunk {}",
                            job.job_id, job.note_id, job.chunk_idx
                        ),
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!("embedding worker error: {error:#}"),
            }
        }

        if processed == 0 {
            tokio::time::sleep(poll_interval).await;
        }
    }
}
