mod labels_api;
mod notes_api;
mod render;
mod system_api;

use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use axum::Router;
use note_attachments::{AttachmentStore, FilesystemAttachmentStore};
use note_embedding::{
    EmbeddingBackendInfo, OpenAiCompatibleConfig, OpenAiCompatibleEmbedder, ProcessWorkerConfig,
    ProcessWorkerRuntime, StubEmbedder, WorkerConfig,
};
use note_pipelines::{
    Context, EmbeddingJobNotifier, FingerprintReconciliation, ProcessEmbeddingJobStatus,
};
use note_server::config::{AttachmentConfig, DatabaseConfig, EmbeddingConfig, RuntimeConfig};
use note_storage::StorageBackend;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
#[cfg(debug_assertions)]
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Child;
#[cfg(debug_assertions)]
use tokio::process::Command;
use tokio::sync::{watch, Notify};

const DEFAULT_EMBEDDING_POLL_MS: u64 = 1000;
const TRASH_RETENTION_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const DEV_FRONTEND_URL: &str = "http://0.0.0.0:6221";

struct NotifyEmbeddingJobs {
    notify: Arc<Notify>,
}

impl EmbeddingJobNotifier for NotifyEmbeddingJobs {
    fn wake(&self) {
        self.notify.notify_one();
    }
}

#[async_trait::async_trait]
trait ProcessRuntimeHandle: Send {
    async fn shutdown(self: Box<Self>);
}

#[async_trait::async_trait]
impl ProcessRuntimeHandle for ProcessWorkerRuntime {
    async fn shutdown(self: Box<Self>) {
        (*self).shutdown().await;
    }
}

struct RunningEmbedding {
    embedder: Arc<dyn note_embedding::Embedder>,
    info: EmbeddingBackendInfo,
    process_runtime: Option<Box<dyn ProcessRuntimeHandle>>,
    wake: Arc<Notify>,
}

async fn start_embedding_runtime(config: &EmbeddingConfig) -> anyhow::Result<RunningEmbedding> {
    match config {
        EmbeddingConfig::Local { model_path } => {
            let worker_config =
                with_resolved_model_path(ProcessWorkerConfig::from_env()?, model_path);
            let process_runtime = ProcessWorkerRuntime::start(worker_config).await?;
            Ok(RunningEmbedding {
                embedder: process_runtime.embedder(),
                info: EmbeddingBackendInfo::local_bge_m3(),
                process_runtime: Some(Box::new(process_runtime)),
                wake: Arc::new(Notify::new()),
            })
        }
        EmbeddingConfig::OpenAi {
            base_url,
            model,
            api_key_env,
            timeout_secs,
            max_retries,
        } => {
            let bearer_token =
                resolve_bearer_token(api_key_env.as_deref(), |name| std::env::var(name).ok())?;
            let info = EmbeddingBackendInfo::openai(model.clone());
            let embedder = OpenAiCompatibleEmbedder::new(OpenAiCompatibleConfig {
                base_url: base_url.clone(),
                model: model.clone(),
                bearer_token,
                timeout: Duration::from_secs(*timeout_secs),
                max_retries: *max_retries,
            })?;
            Ok(RunningEmbedding {
                embedder: Arc::new(embedder),
                info,
                process_runtime: None,
                wake: Arc::new(Notify::new()),
            })
        }
    }
}

fn resolve_bearer_token(
    api_key_env: Option<&str>,
    get_env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<String>> {
    let Some(variable) = api_key_env else {
        return Ok(None);
    };
    match get_env(variable) {
        Some(value) if !value.trim().is_empty() => Ok(Some(value)),
        _ => anyhow::bail!("embedding API key environment variable {variable} is missing or blank"),
    }
}

async fn shutdown_embedding(embedding: &mut RunningEmbedding) {
    if let Some(process_runtime) = embedding.process_runtime.take() {
        process_runtime.shutdown().await;
    }
}

async fn reconcile_started_embedding(
    storage: &dyn StorageBackend,
    embedding: &mut RunningEmbedding,
) -> anyhow::Result<FingerprintReconciliation> {
    let result = note_pipelines::reconcile_embedding_fingerprint(
        storage,
        &embedding.info.fingerprint,
        chrono::Utc::now().timestamp(),
    )
    .await;
    match result {
        Ok(reconciliation) => {
            match reconciliation {
                FingerprintReconciliation::Initialized => {
                    eprintln!("initialized embedding fingerprint")
                }
                FingerprintReconciliation::Unchanged => {
                    eprintln!("embedding fingerprint unchanged")
                }
                FingerprintReconciliation::Regenerated { queued_jobs } => {
                    eprintln!(
                        "embedding fingerprint changed; queued {queued_jobs} regeneration jobs"
                    )
                }
            }
            Ok(reconciliation)
        }
        Err(error) => {
            shutdown_embedding(embedding).await;
            Err(error)
        }
    }
}

fn with_resolved_model_path(
    mut worker_config: ProcessWorkerConfig,
    model_path: &Option<PathBuf>,
) -> ProcessWorkerConfig {
    worker_config.model_path = model_path.clone();
    worker_config
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

fn ensure_data_directories(config: &RuntimeConfig) -> anyhow::Result<()> {
    if let DatabaseConfig::Embed { path } = &config.database {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
    }
    if let AttachmentConfig::Filesystem { path } = &config.attachments {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

async fn build_storage(config: &DatabaseConfig) -> anyhow::Result<Arc<dyn StorageBackend>> {
    match config {
        DatabaseConfig::Embed { path } => Ok(Arc::new(
            note_storage_turso::TursoStorage::open(path).await?,
        )),
        DatabaseConfig::Pg {
            url,
            max_connections,
        } => Ok(Arc::new(
            note_storage_pg::PgStorage::connect(url, *max_connections).await?,
        )),
    }
}

fn open_attachments(config: &AttachmentConfig) -> anyhow::Result<Arc<dyn AttachmentStore>> {
    match config {
        AttachmentConfig::Filesystem { path } => {
            Ok(Arc::new(FilesystemAttachmentStore::new(path.clone())))
        }
        AttachmentConfig::S3 { .. } => {
            anyhow::bail!("S3 attachment adapter is not implemented yet")
        }
    }
}

fn should_start_dev_frontend(
    debug_build: bool,
    http_mode: bool,
    explicit_static_dir: bool,
) -> bool {
    debug_build && http_mode && !explicit_static_dir
}

#[cfg(debug_assertions)]
fn frontend_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../note-frontend")
}

fn start_dev_frontend(enabled: bool) -> anyhow::Result<Option<Child>> {
    if !enabled {
        return Ok(None);
    }

    #[cfg(debug_assertions)]
    {
        let frontend_dir = frontend_dir().canonicalize().map_err(|error| {
            anyhow::anyhow!(
                "resolve frontend directory {}: {error}",
                frontend_dir().display()
            )
        })?;
        let mut command = Command::new("trunk");
        command
            .arg("serve")
            .current_dir(&frontend_dir)
            .stdin(Stdio::null())
            .env_remove("NO_COLOR")
            .kill_on_drop(true);
        command.spawn().map(Some).map_err(|error| {
            anyhow::anyhow!(
                "start `trunk serve` from {}: {error}. Install it with `cargo install --locked trunk`",
                frontend_dir.display()
            )
        })
    }

    #[cfg(not(debug_assertions))]
    Ok(None)
}

async fn wait_for_dev_frontend(child: &mut Option<Child>) -> std::io::Result<ExitStatus> {
    match child {
        Some(child) => child.wait().await,
        None => std::future::pending().await,
    }
}

async fn stop_dev_frontend(child: &mut Option<Child>) {
    let Some(child) = child else {
        return;
    };
    match child.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {
            if let Err(error) = child.start_kill() {
                eprintln!("failed to stop trunk serve: {error}");
            }
        }
        Err(error) => eprintln!("failed to inspect trunk serve: {error}"),
    }
    if let Err(error) = child.wait().await {
        eprintln!("failed to reap trunk serve: {error}");
    }
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

async fn run_trash_retention(ctx: Arc<Context>, mut shutdown: watch::Receiver<bool>) {
    loop {
        match note_pipelines::purge_expired_deleted_notes(&ctx, chrono::Utc::now().timestamp())
            .await
        {
            Ok(count) if count > 0 => eprintln!("permanently deleted {count} expired trash notes"),
            Ok(_) => {}
            Err(error) => eprintln!("trash retention cleanup failed: {error:#}"),
        }

        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(TRASH_RETENTION_CHECK_INTERVAL) => {}
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if arg_value(&args, "--internal-role").as_deref() == Some("embedding-worker") {
        let ipc_name = arg_value(&args, "--ipc-name")
            .ok_or_else(|| anyhow::anyhow!("missing --ipc-name for embedding worker"))?;
        return note_embedding::run_embedding_worker(WorkerConfig::new(ipc_name)?).await;
    }

    let stdio_mode = args.iter().any(|a| a == "--stdio");
    let config = note_server::config::load_runtime_config()?;
    ensure_data_directories(&config)?;
    let storage = build_storage(&config.database).await?;
    let attachments = open_attachments(&config.attachments)?;
    let export_mode = args.iter().any(|a| a == "--export");
    let import_mode = args.iter().any(|a| a == "--import");

    if export_mode {
        let ctx = Context::new(storage.clone(), Arc::new(StubEmbedder), attachments.clone());
        println!("{}", note_pipelines::export_json(&ctx).await?);
        return Ok(());
    }

    if import_mode {
        let ctx = Context::new(storage.clone(), Arc::new(StubEmbedder), attachments.clone());
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
        let mut embedding = start_embedding_runtime(&config.embedding).await?;
        reconcile_started_embedding(storage.as_ref(), &mut embedding).await?;
        let notifier = Arc::new(NotifyEmbeddingJobs {
            notify: embedding.wake.clone(),
        });
        let ctx = Arc::new(Context::with_embedding_job_notifier(
            storage.clone(),
            embedding.embedder.clone(),
            embedding.info.clone(),
            notifier,
            attachments.clone(),
        ));
        let (scheduler_shutdown_tx, scheduler_shutdown_rx) = watch::channel(false);
        let trash_retention = tokio::spawn(run_trash_retention(
            ctx.clone(),
            scheduler_shutdown_rx.clone(),
        ));
        let scheduler = tokio::spawn(run_embedding_scheduler(
            ctx.clone(),
            embedding.wake.clone(),
            scheduler_shutdown_rx,
        ));
        let run_result = note_mcp::run_stdio(ctx).await;
        let _ = scheduler_shutdown_tx.send(true);
        let _ = scheduler.await;
        let _ = trash_retention.await;
        shutdown_embedding(&mut embedding).await;
        run_result?;
    } else {
        let mut embedding = start_embedding_runtime(&config.embedding).await?;
        reconcile_started_embedding(storage.as_ref(), &mut embedding).await?;
        let notifier = Arc::new(NotifyEmbeddingJobs {
            notify: embedding.wake.clone(),
        });
        let ctx = Arc::new(Context::with_embedding_job_notifier(
            storage.clone(),
            embedding.embedder.clone(),
            embedding.info.clone(),
            notifier,
            attachments.clone(),
        ));
        let (scheduler_shutdown_tx, scheduler_shutdown_rx) = watch::channel(false);
        let trash_retention = tokio::spawn(run_trash_retention(
            ctx.clone(),
            scheduler_shutdown_rx.clone(),
        ));
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
            .merge(system_api::system_router())
            .with_state(ctx.clone());
        let max_request_bytes = env_u64("NOTE_MAX_REQUEST_BYTES", 512 * 1024 * 1024);
        let mut app: Router = rest
            .merge(note_mcp::mcp_router(ctx))
            .layer(DefaultBodyLimit::max(max_request_bytes as usize));

        // NOTE_STATIC_DIR is for packaged builds such as Docker. Local debug HTTP runs use the
        // Trunk development server instead, unless an explicit static directory is configured.
        let static_dir = std::env::var("NOTE_STATIC_DIR")
            .ok()
            .filter(|static_dir| !static_dir.is_empty());
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
        let mut dev_frontend = None;
        let run_result = match tokio::net::TcpListener::bind(&bind_addr).await {
            Err(error) => Err(anyhow::Error::new(error)),
            Ok(listener) => match start_dev_frontend(should_start_dev_frontend(
                cfg!(debug_assertions),
                true,
                static_dir.is_some(),
            )) {
                Err(error) => Err(error),
                Ok(child) => {
                    dev_frontend = child;
                    eprintln!("note-server listening on http://{bind_addr}");
                    if dev_frontend.is_some() {
                        eprintln!("frontend dev server listening on {DEV_FRONTEND_URL}");
                    } else if let Some(static_dir) = &static_dir {
                        eprintln!("serving frontend from {static_dir}");
                    } else {
                        eprintln!("API only — set NOTE_STATIC_DIR to serve a frontend bundle");
                    }

                    let server = axum::serve(listener, app);
                    tokio::select! {
                        biased;
                        signal = tokio::signal::ctrl_c() => {
                            if let Err(error) = signal {
                                eprintln!("failed to listen for shutdown signal: {error}");
                            }
                            eprintln!("shutting down note-server");
                            Ok(())
                        }
                        status = wait_for_dev_frontend(&mut dev_frontend) => {
                            match status {
                                Ok(status) => Err(anyhow::anyhow!(
                                    "trunk serve exited unexpectedly: {status}"
                                )),
                                Err(error) => Err(anyhow::anyhow!(
                                    "wait for trunk serve: {error}"
                                )),
                            }
                        }
                        result = server => result.map_err(anyhow::Error::new),
                    }
                }
            },
        };
        stop_dev_frontend(&mut dev_frontend).await;
        let _ = scheduler_shutdown_tx.send(true);
        let _ = scheduler.await;
        let _ = trash_retention.await;
        shutdown_embedding(&mut embedding).await;
        run_result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_storage::{
        BackendInfo, StorageError, StorageErrorKind, StorageResult, StorageSession,
        StorageTransaction, TransactionMode,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn bearer_token_resolution_is_optional_and_rejects_missing_or_blank_values() {
        assert_eq!(
            resolve_bearer_token(None, |_| panic!("no environment read expected")).unwrap(),
            None
        );
        assert_eq!(
            resolve_bearer_token(Some("EMBEDDING_API_KEY"), |name| {
                (name == "EMBEDDING_API_KEY").then(|| "secret-token".into())
            })
            .unwrap(),
            Some("secret-token".into())
        );

        for value in [None, Some(" \t\n".into())] {
            let error =
                resolve_bearer_token(Some("EMBEDDING_API_KEY"), |_| value.clone()).unwrap_err();
            let rendered = error.to_string();
            assert!(rendered.contains("EMBEDDING_API_KEY"), "{rendered}");
            assert!(!rendered.contains("unrelated-secret"), "{rendered}");
        }
    }

    #[tokio::test]
    async fn remote_runtime_embeds_without_a_local_process() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_json(serde_json::json!({
                "model": "bge-m3",
                "input": ["remote request"],
                "encoding_format": "float"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "index": 0,
                    "embedding": vec![0.25_f32; note_embedding::DEFAULT_EMBEDDING_DIMENSION]
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let running = start_embedding_runtime(&EmbeddingConfig::OpenAi {
            base_url: server.uri(),
            model: "bge-m3".into(),
            api_key_env: None,
            timeout_secs: 1,
            max_retries: 0,
        })
        .await
        .unwrap();

        assert!(running.process_runtime.is_none());
        assert_eq!(
            running.info,
            note_embedding::EmbeddingBackendInfo::openai("bge-m3")
        );
        assert_eq!(
            running.embedder.embed("remote request").await.unwrap(),
            vec![0.25_f32; note_embedding::DEFAULT_EMBEDDING_DIMENSION]
        );
    }

    struct FakeRuntimeHandle {
        shutdown: Arc<AtomicBool>,
    }

    struct FailingReconciliationBackend;

    #[async_trait::async_trait]
    impl StorageBackend for FailingReconciliationBackend {
        async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
            Err(StorageError::new(
                StorageErrorKind::Operation,
                "session is not used",
            ))
        }

        async fn begin(
            &self,
            _mode: TransactionMode,
        ) -> StorageResult<Box<dyn StorageTransaction>> {
            Err(StorageError::new(
                StorageErrorKind::Operation,
                "controlled reconciliation failure",
            ))
        }

        async fn info(&self) -> StorageResult<BackendInfo> {
            Err(StorageError::new(
                StorageErrorKind::Operation,
                "info is not used",
            ))
        }
    }

    #[async_trait::async_trait]
    impl ProcessRuntimeHandle for FakeRuntimeHandle {
        async fn shutdown(self: Box<Self>) {
            self.shutdown.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn reconciliation_failure_shuts_down_a_started_local_runtime() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let storage: Arc<dyn StorageBackend> = Arc::new(FailingReconciliationBackend);
        let mut embedding = RunningEmbedding {
            embedder: Arc::new(StubEmbedder),
            info: note_embedding::EmbeddingBackendInfo::local_bge_m3(),
            process_runtime: Some(Box::new(FakeRuntimeHandle {
                shutdown: shutdown.clone(),
            })),
            wake: Arc::new(Notify::new()),
        };

        let error = reconcile_started_embedding(storage.as_ref(), &mut embedding)
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("controlled reconciliation failure"));
        assert!(embedding.process_runtime.is_none());
        assert!(shutdown.load(Ordering::SeqCst));
    }

    async fn reject_postgres_startup(listener: tokio::net::TcpListener) -> std::io::Result<()> {
        const SSL_REQUEST_CODE: u32 = 80_877_103;

        let (mut stream, _) = listener.accept().await?;
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header).await?;
        let packet_len = u32::from_be_bytes(header) as usize;
        if packet_len < 8 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid PostgreSQL startup packet length",
            ));
        }
        let mut body = vec![0_u8; packet_len - 4];
        stream.read_exact(&mut body).await?;

        let request_code = u32::from_be_bytes(body[..4].try_into().unwrap());
        if request_code == SSL_REQUEST_CODE {
            stream.write_all(b"N").await?;
            stream.read_exact(&mut header).await?;
            let packet_len = u32::from_be_bytes(header) as usize;
            if packet_len < 8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid PostgreSQL startup packet length",
                ));
            }
            body.resize(packet_len - 4, 0);
            stream.read_exact(&mut body).await?;
        }

        let mut payload = b"SERROR\0C28P01\0Mcontrolled authentication rejection\0".to_vec();
        payload.push(0);
        let mut error_packet = Vec::with_capacity(payload.len() + 5);
        error_packet.push(b'E');
        error_packet.extend_from_slice(&((payload.len() + 4) as u32).to_be_bytes());
        error_packet.extend_from_slice(&payload);
        stream.write_all(&error_packet).await?;
        stream.shutdown().await
    }

    async fn await_postgres_peer(mut peer: tokio::task::JoinHandle<std::io::Result<()>>) {
        match tokio::time::timeout(Duration::from_secs(2), &mut peer).await {
            Ok(result) => result
                .expect("controlled PostgreSQL peer task must not panic")
                .expect("controlled PostgreSQL peer must complete its exchange"),
            Err(_) => {
                peer.abort();
                let _ = peer.await;
                panic!("controlled PostgreSQL peer did not finish");
            }
        }
    }

    #[test]
    fn data_directories_are_created_only_for_local_adapters() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("dev-data/notes.db");
        let attachments_dir = temp.path().join("dev-data/attachments");
        let local = RuntimeConfig {
            config_path: temp.path().join("config.toml"),
            database: DatabaseConfig::Embed {
                path: db_path.clone(),
            },
            embedding: EmbeddingConfig::Local { model_path: None },
            attachments: AttachmentConfig::Filesystem {
                path: attachments_dir.clone(),
            },
        };

        ensure_data_directories(&local).unwrap();

        assert!(db_path.parent().unwrap().is_dir());
        assert!(attachments_dir.is_dir());
        assert!(!db_path.exists());

        let external_root = temp.path().join("external-only");
        let external = RuntimeConfig {
            config_path: temp.path().join("config.toml"),
            database: DatabaseConfig::Pg {
                url: "postgresql://localhost/notes".into(),
                max_connections: 10,
            },
            embedding: EmbeddingConfig::OpenAi {
                base_url: "http://localhost:8080".into(),
                model: "bge-m3".into(),
                api_key_env: None,
                timeout_secs: 30,
                max_retries: 3,
            },
            attachments: AttachmentConfig::S3 {
                bucket: "notes".into(),
                prefix: external_root.to_string_lossy().into_owned(),
                region: None,
                endpoint: None,
                force_path_style: false,
            },
        };

        ensure_data_directories(&external).unwrap();
        assert!(!external_root.exists());
    }

    #[tokio::test]
    async fn embed_storage_opens_only_the_configured_turso_path() {
        let temp = tempfile::tempdir().unwrap();
        let configured_path = temp.path().join("configured/notes.db");
        let default_path = temp.path().join("dev-data/notes.db");
        let config = RuntimeConfig {
            config_path: temp.path().join("config.toml"),
            database: DatabaseConfig::Embed {
                path: configured_path.clone(),
            },
            embedding: EmbeddingConfig::Local { model_path: None },
            attachments: AttachmentConfig::Filesystem {
                path: temp.path().join("attachments"),
            },
        };

        ensure_data_directories(&config).unwrap();
        let storage = build_storage(&config.database).await.unwrap();
        storage.session().await.unwrap();

        assert!(configured_path.exists());
        assert!(!default_path.exists());
    }

    #[tokio::test]
    async fn pg_connection_errors_are_bounded_and_do_not_render_credentials() {
        const USERNAME: &str = "do-not-render-this-user";
        const PASSWORD: &str = "do-not-render-this-secret";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(reject_postgres_startup(listener));
        let url = format!("postgresql://{USERNAME}:{PASSWORD}@{address}/notes");

        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            build_storage(&DatabaseConfig::Pg {
                url: url.clone(),
                max_connections: 1,
            }),
        )
        .await;
        let elapsed = started.elapsed();
        await_postgres_peer(peer).await;
        let error = result
            .expect("controlled PostgreSQL rejection must finish within five seconds")
            .err()
            .expect("controlled PostgreSQL peer must reject the connection");
        let rendered = format!("{error:#}");

        assert!(elapsed < Duration::from_secs(5), "elapsed: {elapsed:?}");
        assert_eq!(rendered, "connect to PostgreSQL");
        assert!(!rendered.contains(&url), "{rendered}");
        assert!(!rendered.contains(USERNAME), "{rendered}");
        assert!(!rendered.contains(PASSWORD), "{rendered}");
    }

    #[test]
    fn reserved_external_adapters_return_precise_errors() {
        let s3 = open_attachments(&AttachmentConfig::S3 {
            bucket: "notes".into(),
            prefix: String::new(),
            region: None,
            endpoint: None,
            force_path_style: false,
        })
        .err()
        .expect("S3 must remain reserved");
        assert_eq!(
            s3.to_string(),
            "S3 attachment adapter is not implemented yet"
        );
    }

    #[test]
    fn resolved_local_model_path_overwrites_process_worker_environment_value() {
        let mut worker_config = ProcessWorkerConfig::from_env().unwrap();
        worker_config.model_path = Some("ambient-model.onnx".into());
        let resolved = Some(PathBuf::from("/config/models/bge-m3.onnx"));

        let worker_config = with_resolved_model_path(worker_config, &resolved);

        assert_eq!(worker_config.model_path, resolved);

        let mut worker_config = ProcessWorkerConfig::from_env().unwrap();
        worker_config.model_path = Some("ambient-model.onnx".into());
        let worker_config = with_resolved_model_path(worker_config, &None);
        assert_eq!(worker_config.model_path, None);
    }

    #[test]
    fn dev_frontend_only_starts_for_debug_http_without_static_dir() {
        assert!(should_start_dev_frontend(true, true, false));
        assert!(!should_start_dev_frontend(false, true, false));
        assert!(!should_start_dev_frontend(true, false, false));
        assert!(!should_start_dev_frontend(true, true, true));
    }

    #[test]
    fn frontend_config_uses_the_expected_dev_address() {
        let config = std::fs::read_to_string(frontend_dir().join("Trunk.toml")).unwrap();
        assert!(config.contains("addresses = [\"0.0.0.0\"]"));
        assert!(config.contains("port = 6221"));
        assert_eq!(DEV_FRONTEND_URL, "http://0.0.0.0:6221");
    }
}
