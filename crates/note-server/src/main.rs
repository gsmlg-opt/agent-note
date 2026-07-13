mod labels_api;
mod notes_api;
mod render;

use axum::response::IntoResponse;
use axum::Router;
use note_embedding::{ProcessWorkerConfig, ProcessWorkerRuntime, StubEmbedder, WorkerConfig};
use note_pipelines::{Context, EmbeddingJobNotifier, ProcessEmbeddingJobStatus};
use note_storage::Storage;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Notify};

const DEFAULT_EMBEDDING_POLL_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbeddingExecutionMode {
    Process,
    Thread,
    Remote,
}

impl EmbeddingExecutionMode {
    fn from_env() -> anyhow::Result<Self> {
        match std::env::var("NOTE_EMBEDDING_MODE")
            .unwrap_or_else(|_| "process".to_string())
            .as_str()
        {
            "process" => Ok(Self::Process),
            "thread" => Ok(Self::Thread),
            "remote" => Ok(Self::Remote),
            other => anyhow::bail!("unknown NOTE_EMBEDDING_MODE={other}"),
        }
    }
}

struct NotifyEmbeddingJobs {
    notify: Arc<Notify>,
}

impl EmbeddingJobNotifier for NotifyEmbeddingJobs {
    fn wake(&self) {
        self.notify.notify_one();
    }
}

struct RunningEmbedding {
    embedder: Arc<dyn note_embedding::Embedder>,
    process_runtime: Option<ProcessWorkerRuntime>,
    wake: Arc<Notify>,
}

async fn start_embedding_runtime() -> anyhow::Result<RunningEmbedding> {
    match EmbeddingExecutionMode::from_env()? {
        EmbeddingExecutionMode::Process => {
            let process_runtime =
                ProcessWorkerRuntime::start(ProcessWorkerConfig::from_env()?).await?;
            Ok(RunningEmbedding {
                embedder: process_runtime.embedder(),
                process_runtime: Some(process_runtime),
                wake: Arc::new(Notify::new()),
            })
        }
        EmbeddingExecutionMode::Thread => {
            anyhow::bail!("NOTE_EMBEDDING_MODE=thread is reserved but not implemented yet")
        }
        EmbeddingExecutionMode::Remote => {
            anyhow::bail!("NOTE_EMBEDDING_MODE=remote is reserved but not implemented yet")
        }
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find_map(|window| (window[0] == name).then(|| window[1].clone()))
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn attachments_dir_from_env(db_path: &str) -> PathBuf {
    std::env::var("NOTE_ATTACHMENTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(db_path)
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .join("attachments")
        })
}

async fn run_embedding_scheduler(
    ctx: Arc<Context>,
    wake: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    match note_pipelines::requeue_processing_embedding_jobs(&ctx).await {
        Ok(count) if count > 0 => eprintln!("requeued {count} interrupted embedding jobs"),
        Ok(_) => {}
        Err(error) => eprintln!("requeue embedding jobs failed: {error:#}"),
    }
    match note_pipelines::enqueue_missing_chunk_embeddings(&ctx).await {
        Ok(count) if count > 0 => eprintln!("queued {count} missing chunk embeddings"),
        Ok(_) => {}
        Err(error) => eprintln!("enqueue missing embedding jobs failed: {error:#}"),
    }

    let poll_interval =
        Duration::from_millis(env_u64("NOTE_EMBEDDING_POLL_MS", DEFAULT_EMBEDDING_POLL_MS));
    loop {
        if *shutdown.borrow() {
            break;
        }
        match note_pipelines::process_next_embedding_job(&ctx).await {
            Ok(Some(job)) => {
                match job.status {
                    ProcessEmbeddingJobStatus::Completed => {
                        eprintln!("embedded note {} chunk {}", job.note_id, job.chunk_idx)
                    }
                    ProcessEmbeddingJobStatus::Stale => eprintln!(
                        "discarded stale embedding job {} for note {} chunk {}",
                        job.job_id, job.note_id, job.chunk_idx
                    ),
                    ProcessEmbeddingJobStatus::Failed => eprintln!(
                        "embedding job {} failed for note {} chunk {}",
                        job.job_id, job.note_id, job.chunk_idx
                    ),
                }
                continue;
            }
            Ok(None) => {}
            Err(error) => eprintln!("embedding scheduler error: {error:#}"),
        }

        tokio::select! {
            _ = shutdown.changed() => {},
            _ = wake.notified() => {},
            _ = tokio::time::sleep(poll_interval) => {},
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if arg_value(&args, "--internal-role").as_deref() == Some("embedding-worker") {
        let ipc_name = arg_value(&args, "--ipc-name")
            .ok_or_else(|| anyhow::anyhow!("missing --ipc-name for embedding worker"))?;
        return note_embedding::run_embedding_worker(WorkerConfig::new(ipc_name)).await;
    }

    let stdio_mode = args.iter().any(|a| a == "--stdio");
    let db_path = std::env::var("NOTE_DB_PATH").unwrap_or_else(|_| "notes.db".to_string());
    let attachments_dir = attachments_dir_from_env(&db_path);
    let export_mode = args.iter().any(|a| a == "--export");
    let import_mode = args.iter().any(|a| a == "--import");

    if export_mode {
        let storage = Storage::open_local(&db_path).await?;
        let ctx = Context::with_attachment_dir(
            Arc::new(storage),
            Arc::new(StubEmbedder),
            attachments_dir,
        );
        println!("{}", note_pipelines::export_json(&ctx).await?);
        return Ok(());
    }

    if import_mode {
        let storage = Storage::open_local(&db_path).await?;
        let ctx = Context::with_attachment_dir(
            Arc::new(storage),
            Arc::new(StubEmbedder),
            attachments_dir,
        );
        let mut input = String::new();
        std::io::stdin().lock().read_to_string(&mut input)?;
        let stats = note_pipelines::import_json(&ctx, &input).await?;
        eprintln!(
            "import: {} notes added, {} skipped, {} label keys added, {} embedding jobs queued",
            stats.notes_added,
            stats.notes_skipped,
            stats.label_keys_added,
            stats.embedding_jobs_queued
        );
        return Ok(());
    }

    if stdio_mode {
        // stdio is single-client — no connection pool needed (docs/design.md §9).
        let storage = Storage::open_local(&db_path).await?;
        let embedding = start_embedding_runtime().await?;
        let notifier = Arc::new(NotifyEmbeddingJobs {
            notify: embedding.wake.clone(),
        });
        let ctx = Arc::new(Context::with_embedding_job_notifier_and_attachment_dir(
            Arc::new(storage),
            embedding.embedder.clone(),
            notifier,
            attachments_dir.clone(),
        ));
        let (scheduler_shutdown_tx, scheduler_shutdown_rx) = watch::channel(false);
        let scheduler = tokio::spawn(run_embedding_scheduler(
            ctx.clone(),
            embedding.wake.clone(),
            scheduler_shutdown_rx,
        ));
        note_mcp::run_stdio(ctx).await?;
        let _ = scheduler_shutdown_tx.send(true);
        let _ = scheduler.await;
        if let Some(process_runtime) = embedding.process_runtime {
            process_runtime.shutdown().await;
        }
    } else {
        // Axum is the only process that needs connection pooling (docs/design.md §2).
        // TODO: size the pool deliberately once concurrency requirements are clearer (docs/design.md §9
        // open decision) — starting with a single shared Storage handle is a placeholder, not a final answer.
        let storage = Storage::open_local(&db_path).await?;
        let embedding = start_embedding_runtime().await?;
        let notifier = Arc::new(NotifyEmbeddingJobs {
            notify: embedding.wake.clone(),
        });
        let ctx = Arc::new(Context::with_embedding_job_notifier_and_attachment_dir(
            Arc::new(storage),
            embedding.embedder.clone(),
            notifier,
            attachments_dir.clone(),
        ));
        let (scheduler_shutdown_tx, scheduler_shutdown_rx) = watch::channel(false);
        let scheduler = tokio::spawn(run_embedding_scheduler(
            ctx.clone(),
            embedding.wake.clone(),
            scheduler_shutdown_rx,
        ));

        // notes_router()/labels_router() are Router<Arc<Context>> — applying .with_state converts them
        // to Router<()>, which can then merge with mcp_router() (already Router<()>, self-stated).
        let rest = Router::new()
            .merge(notes_api::notes_router())
            .merge(labels_api::labels_router())
            .with_state(ctx.clone());
        let mut app: Router = rest.merge(note_mcp::mcp_router(ctx));

        // If NOTE_STATIC_DIR is set (e.g. the Docker image points it at the built wasm bundle),
        // serve those static files for any path the API/MCP routes don't claim, falling back to
        // index.html. In debug builds, `cargo run` also serves a previously built Trunk bundle.
        let static_dir = match std::env::var("NOTE_STATIC_DIR") {
            Ok(static_dir) if !static_dir.is_empty() => Some(static_dir),
            _ if cfg!(debug_assertions)
                && std::path::Path::new("crates/note-frontend/dist").is_dir() =>
            {
                Some("crates/note-frontend/dist".to_string())
            }
            _ => None,
        };
        if let Some(static_dir) = &static_dir {
            // SPA fallback for any path the API/MCP routes don't claim: serve a real static asset
            // when one exists at that path, otherwise return index.html (200) so client-side routes
            // like /new and /labels boot on a direct load or refresh. (ServeDir's not_found_service
            // would serve index.html but with a 404 status, wrong for a valid SPA route.)
            let static_dir = static_dir.clone();
            let index_html =
                std::fs::read_to_string(format!("{static_dir}/index.html")).unwrap_or_default();
            app = app.fallback(move |uri: axum::http::Uri| {
                let static_dir = static_dir.clone();
                let index_html = index_html.clone();
                async move {
                    let path = uri.path().trim_start_matches('/');
                    if !path.is_empty() && !path.contains("..") {
                        if let Ok(bytes) = tokio::fs::read(format!("{static_dir}/{path}")).await {
                            let content_type = match path.rsplit('.').next() {
                                Some("js") => "text/javascript",
                                Some("wasm") => "application/wasm",
                                Some("css") => "text/css",
                                Some("html") => "text/html; charset=utf-8",
                                Some("json") => "application/json",
                                Some("svg") => "image/svg+xml",
                                Some("ico") => "image/x-icon",
                                _ => "application/octet-stream",
                            };
                            return ([(axum::http::header::CONTENT_TYPE, content_type)], bytes)
                                .into_response();
                        }
                    }
                    axum::response::Html(index_html).into_response()
                }
            });
        }

        // Default to loopback: a fully-offline, unauthenticated personal app (docs/design.md §1),
        // and note-mcp's /mcp router defaults to a loopback-only Host allowlist. NOTE_BIND_ADDR
        // overrides it — the Docker image sets 0.0.0.0:6222 so the container is reachable via `-p`
        // (container-network isolation makes that safe; exposing it to your LAN is your `-p` choice).
        let bind_addr =
            std::env::var("NOTE_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:6222".to_string());
        eprintln!("note-server listening on http://{bind_addr}");
        if let Some(static_dir) = &static_dir {
            eprintln!("serving frontend from {static_dir}");
        } else {
            eprintln!(
                "API only — run `trunk build` then re-run for the UI, or `trunk serve` for hot reload"
            );
        }
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
        let server = axum::serve(listener, app);
        tokio::select! {
            result = server => result?,
            signal = tokio::signal::ctrl_c() => {
                if let Err(error) = signal {
                    eprintln!("failed to listen for shutdown signal: {error}");
                }
                eprintln!("shutting down note-server");
            }
        }
        let _ = scheduler_shutdown_tx.send(true);
        let _ = scheduler.await;
        if let Some(process_runtime) = embedding.process_runtime {
            process_runtime.shutdown().await;
        }
    }
    Ok(())
}
