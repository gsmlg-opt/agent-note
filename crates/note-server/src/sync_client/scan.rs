//! Source pagination and session-owned disk spooling, independent of network speed.
use anyhow::{Context as _, Result};
use note_pipelines::{
    sync::{self, SyncCandidate},
    Context,
};
use std::sync::Arc;
use tokio::{sync::watch, task::JoinHandle};

pub const PAGE_SIZE: usize = 32;
#[derive(Clone, Default)]
pub struct SourceProgress {
    pub pages_scanned: u64,
    pub notes_scanned: usize,
    pub source_cursor: Option<String>,
    pub scan_complete: bool,
    pub last_progress_at: i64,
    pub failed: bool,
}
pub struct ScanSpool {
    directory: Arc<tempfile::TempDir>,
    producer: JoinHandle<()>,
    progress: watch::Receiver<SourceProgress>,
    next_page: u64,
}
impl Drop for ScanSpool {
    fn drop(&mut self) {
        self.producer.abort();
    }
}
impl ScanSpool {
    pub fn start(ctx: Context, pending_only: bool) -> Result<Self> {
        let directory = Arc::new(tempfile::tempdir().context("cannot create sync scan spool")?);
        let directory_worker = directory.clone();
        let (progress_tx, progress) = watch::channel(SourceProgress::default());
        let producer = tokio::spawn(async move {
            let mut state = SourceProgress::default();
            let result = async {
                let mut source = sync::start_sync_candidate_scan(&ctx, pending_only).await?;
                loop {
                    let page = source.next_page(PAGE_SIZE).await?;
                    if page.is_empty() {
                        break;
                    }
                    let temporary = directory_worker
                        .path()
                        .join(format!("{}.partial", state.pages_scanned));
                    let complete = directory_worker
                        .path()
                        .join(format!("{}.json", state.pages_scanned));
                    let owner = directory_worker.clone();
                    let bytes = serde_json::to_vec(&page)?;
                    tokio::task::spawn_blocking(move || {
                        let _owner = owner; // Retain the directory even if its async producer is cancelled.
                        std::fs::write(&temporary, bytes)?;
                        std::fs::rename(&temporary, &complete)
                    })
                    .await??;
                    state.pages_scanned += 1;
                    state.notes_scanned += page.len();
                    state.source_cursor = page
                        .last()
                        .map(|candidate| candidate.descriptor.note_id.clone());
                    state.last_progress_at = chrono::Utc::now().timestamp_millis();
                    progress_tx.send_replace(state.clone());
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            state.failed = result.is_err();
            state.scan_complete = result.is_ok();
            state.last_progress_at = chrono::Utc::now().timestamp_millis();
            progress_tx.send_replace(state);
        });
        Ok(Self {
            directory,
            producer,
            progress,
            next_page: 0,
        })
    }
    pub fn subscribe(&self) -> watch::Receiver<SourceProgress> {
        self.progress.clone()
    }
    pub fn progress(&self) -> SourceProgress {
        self.progress.borrow().clone()
    }
    pub async fn next(&mut self) -> Result<Option<Vec<SyncCandidate>>> {
        loop {
            let state = self.progress();
            if self.next_page < state.pages_scanned {
                let path = self
                    .directory
                    .path()
                    .join(format!("{}.json", self.next_page));
                let owner = self.directory.clone();
                let page = tokio::task::spawn_blocking(move || {
                    let _owner = owner;
                    let page = serde_json::from_slice(&std::fs::read(&path)?)?;
                    std::fs::remove_file(path)?;
                    Ok::<Vec<SyncCandidate>, anyhow::Error>(page)
                })
                .await??;
                self.next_page += 1;
                return Ok(Some(page));
            }
            anyhow::ensure!(
                !state.failed,
                "source scan failed; queued notes remain pending"
            );
            if state.scan_complete {
                return Ok(None);
            }
            self.progress
                .changed()
                .await
                .context("sync scan producer stopped")?;
        }
    }
}

/// Empty per-note markers retain exact round de-duplication without a cohort-sized RAM set.
pub async fn mark(directory: &std::path::Path, run: &str, note: &str, kind: &str) -> Result<bool> {
    anyhow::ensure!(
        matches!(kind, "resolved" | "blocked"),
        "invalid progress marker"
    );
    uuid::Uuid::parse_str(run)?;
    uuid::Uuid::parse_str(note)?;
    let path = directory.join(format!("{run}-{note}-{kind}"));
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}
pub async fn unmark(
    directory: &std::path::Path,
    run: &str,
    note: &str,
    kind: &str,
) -> Result<bool> {
    anyhow::ensure!(
        matches!(kind, "resolved" | "blocked"),
        "invalid progress marker"
    );
    uuid::Uuid::parse_str(run)?;
    uuid::Uuid::parse_str(note)?;
    let path = directory.join(format!("{run}-{note}-{kind}"));
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
