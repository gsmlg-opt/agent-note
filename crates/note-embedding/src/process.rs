use crate::rpc::{
    default_capabilities, unix_time_ms, EmbedInput, EmbedRequest, HandshakeRequest, RpcErrorKind,
    DEFAULT_EMBEDDING_DIMENSION, DEFAULT_MAX_RESPONSE_BYTES,
};
use crate::worker::{connect_client, embedding_threads_from_env};
use crate::{DenseVector, Embedder, EmbeddingRpcClient, SparseVector};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Notify, RwLock};

#[derive(Debug, Clone)]
pub struct ProcessWorkerConfig {
    pub worker_exe: PathBuf,
    pub ipc_name: String,
    pub model_path: Option<PathBuf>,
    pub queue_capacity: usize,
    pub embedding_threads: usize,
    pub max_restarts: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    pub handshake: HandshakeRequest,
    pub extra_env: Vec<(String, String)>,
}

impl ProcessWorkerConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            worker_exe: std::env::current_exe()?,
            ipc_name: default_ipc_name(),
            model_path: std::env::var("NOTE_MODEL_PATH")
                .ok()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            queue_capacity: env_usize("NOTE_EMBEDDING_WORKER_QUEUE", 8),
            embedding_threads: embedding_threads_from_env()?,
            max_restarts: env_usize("NOTE_EMBEDDING_WORKER_MAX_RESTARTS", 5),
            connect_timeout: Duration::from_secs(env_u64(
                "NOTE_EMBEDDING_WORKER_CONNECT_TIMEOUT_SECS",
                10,
            )),
            request_timeout: Duration::from_secs(env_u64(
                "NOTE_EMBEDDING_WORKER_REQUEST_TIMEOUT_SECS",
                30,
            )),
            backoff_initial: Duration::from_millis(env_u64(
                "NOTE_EMBEDDING_WORKER_BACKOFF_INITIAL_MS",
                200,
            )),
            backoff_max: Duration::from_millis(env_u64(
                "NOTE_EMBEDDING_WORKER_BACKOFF_MAX_MS",
                5_000,
            )),
            handshake: HandshakeRequest {
                protocol_version: crate::rpc::IPC_PROTOCOL_VERSION,
                expected_model_id: None,
                expected_dimension: DEFAULT_EMBEDDING_DIMENSION,
                required_capabilities: default_capabilities(),
            },
            extra_env: Vec::new(),
        })
    }
}

pub struct ProcessWorkerRuntime {
    embedder: Arc<ProcessEmbedder>,
    shutdown: watch::Sender<bool>,
    supervisor: tokio::task::JoinHandle<()>,
}

impl ProcessWorkerRuntime {
    pub async fn start(config: ProcessWorkerConfig) -> anyhow::Result<Self> {
        let embedder = Arc::new(ProcessEmbedder::new(config.request_timeout));
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let supervisor_embedder = embedder.clone();
        let supervisor = tokio::spawn(async move {
            run_supervisor(config, supervisor_embedder, shutdown_rx, ready_tx).await;
        });

        match ready_rx.await {
            Ok(Ok(())) => Ok(Self {
                embedder,
                shutdown,
                supervisor,
            }),
            Ok(Err(error)) => {
                let _ = shutdown.send(true);
                Err(anyhow::anyhow!(error))
            }
            Err(_) => {
                let _ = shutdown.send(true);
                Err(anyhow::anyhow!(
                    "embedding worker supervisor stopped before ready"
                ))
            }
        }
    }

    pub fn embedder(&self) -> Arc<dyn Embedder> {
        self.embedder.clone()
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let _ = self.supervisor.await;
    }
}

pub struct ProcessEmbedder {
    client: RwLock<Option<EmbeddingRpcClient>>,
    ready: Notify,
    request_timeout: Duration,
}

impl ProcessEmbedder {
    fn new(request_timeout: Duration) -> Self {
        Self {
            client: RwLock::new(None),
            ready: Notify::new(),
            request_timeout,
        }
    }

    async fn set_client(&self, client: EmbeddingRpcClient) {
        *self.client.write().await = Some(client);
        self.ready.notify_waiters();
    }

    async fn clear_client(&self) {
        *self.client.write().await = None;
    }

    async fn wait_client(&self) -> anyhow::Result<EmbeddingRpcClient> {
        let deadline = Instant::now() + self.request_timeout;
        loop {
            if let Some(client) = self.client.read().await.clone() {
                return Ok(client);
            }
            let now = Instant::now();
            if now >= deadline {
                anyhow::bail!("embedding worker is not ready");
            }
            let remaining = deadline.saturating_duration_since(now);
            if tokio::time::timeout(remaining, self.ready.notified())
                .await
                .is_err()
            {
                anyhow::bail!("embedding worker is not ready");
            }
        }
    }
}

#[async_trait::async_trait]
impl Embedder for ProcessEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        let client = self.wait_client().await?;
        let request_id = format!("embed-{}-{}", std::process::id(), unix_time_ms());
        let deadline_unix_ms =
            unix_time_ms().saturating_add(self.request_timeout.as_millis() as u64);
        let request = EmbedRequest {
            request_id,
            deadline_unix_ms: Some(deadline_unix_ms),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            inputs: vec![EmbedInput {
                job_id: 0,
                text: text.to_string(),
            }],
        };
        let mut ctx = tarpc::context::current();
        ctx.deadline = Instant::now() + self.request_timeout;
        let response = client
            .embed(ctx, request)
            .await
            .map_err(|error| anyhow::anyhow!("embedding rpc transport error: {error}"))?
            .map_err(|error| anyhow::anyhow!("embedding worker error: {error}"))?;
        let output = response
            .outputs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("embedding worker returned no output"))?;
        match output.result {
            Ok(vectors) => Ok(vectors.into_parts()),
            Err(error) => Err(anyhow::anyhow!("embedding worker item error: {error}")),
        }
    }
}

async fn run_supervisor(
    config: ProcessWorkerConfig,
    embedder: Arc<ProcessEmbedder>,
    mut shutdown: watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    let mut ready = Some(ready);
    let mut restart_count = 0usize;
    let mut backoff = config.backoff_initial;

    loop {
        if *shutdown.borrow() {
            break;
        }
        if restart_count > config.max_restarts {
            let message = format!(
                "embedding worker exceeded restart limit ({})",
                config.max_restarts
            );
            if let Some(tx) = ready.take() {
                let _ = tx.send(Err(message.clone()));
            }
            eprintln!("{message}");
            break;
        }

        let child = match spawn_worker(&config) {
            Ok(child) => child,
            Err(error) => {
                if let Some(tx) = ready.take() {
                    let _ = tx.send(Err(format!("spawn embedding worker: {error:#}")));
                }
                break;
            }
        };

        let mut child = child;
        match connect_and_handshake(&config).await {
            Ok(client) => {
                embedder.set_client(client.clone()).await;
                if let Some(tx) = ready.take() {
                    let _ = tx.send(Ok(()));
                }
                let should_shutdown =
                    wait_for_worker_exit_or_shutdown(&mut child, client, &mut shutdown).await;
                embedder.clear_client().await;
                if should_shutdown {
                    break;
                }
            }
            Err(error) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                if let Some(tx) = ready.take() {
                    let _ = tx.send(Err(format!("handshake embedding worker: {error:#}")));
                    break;
                }
            }
        }

        restart_count += 1;
        eprintln!(
            "embedding worker exited unexpectedly; restarting in {}ms",
            backoff.as_millis()
        );
        tokio::select! {
            _ = shutdown.changed() => {},
            _ = tokio::time::sleep(backoff) => {},
        }
        backoff = (backoff * 2).min(config.backoff_max);
    }
}

fn spawn_worker(config: &ProcessWorkerConfig) -> anyhow::Result<Child> {
    let mut command = Command::new(&config.worker_exe);
    command
        .arg("--internal-role")
        .arg("embedding-worker")
        .arg("--ipc-name")
        .arg(&config.ipc_name)
        .env(
            "NOTE_EMBEDDING_WORKER_QUEUE",
            config.queue_capacity.to_string(),
        )
        .env(
            "NOTE_EMBEDDING_THREADS",
            config.embedding_threads.to_string(),
        );
    if let Some(model_path) = &config.model_path {
        command.env("NOTE_MODEL_PATH", model_path);
    } else {
        command.env_remove("NOTE_MODEL_PATH");
    }
    for (key, value) in &config.extra_env {
        command.env(key, value);
    }
    Ok(command.spawn()?)
}

async fn connect_and_handshake(config: &ProcessWorkerConfig) -> anyhow::Result<EmbeddingRpcClient> {
    let deadline = Instant::now() + config.connect_timeout;
    loop {
        match connect_client(&config.ipc_name).await {
            Ok(client) => {
                let mut ctx = tarpc::context::current();
                ctx.deadline = Instant::now() + config.connect_timeout;
                match client.handshake(ctx, config.handshake.clone()).await {
                    Ok(Ok(_)) => return Ok(client),
                    Ok(Err(error)) if error.kind == RpcErrorKind::ProtocolMismatch => {
                        return Err(anyhow::anyhow!(error));
                    }
                    Ok(Err(error)) => return Err(anyhow::anyhow!(error)),
                    Err(error) => {
                        return Err(anyhow::anyhow!("handshake transport error: {error}"))
                    }
                }
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn wait_for_worker_exit_or_shutdown(
    child: &mut Child,
    client: EmbeddingRpcClient,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        status = child.wait() => {
            match status {
                Ok(status) => eprintln!("embedding worker exited: {status}"),
                Err(error) => eprintln!("embedding worker wait error: {error}"),
            }
            false
        }
        _ = shutdown.changed() => {
            if *shutdown.borrow() {
                let mut ctx = tarpc::context::current();
                ctx.deadline = Instant::now() + Duration::from_secs(2);
                let _ = client.shutdown(ctx).await;
                if tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_err() {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
                true
            } else {
                false
            }
        }
    }
}

fn default_ipc_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("agent-note-embedding-{}-{nanos}", std::process::id())
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
