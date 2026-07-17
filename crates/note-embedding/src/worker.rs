use crate::rpc::{
    unix_time_ms, validate_handshake, EmbedRequest, EmbedResponse, EmbeddingRpc,
    EmbeddingRpcClient, HandshakeRequest, HandshakeResponse, HealthResponse, ModelInfo, RpcError,
    RpcErrorKind, WorkerLimits,
};
use crate::{BoundedEmbedder, Embedder, OrtEmbedder, StubEmbedder};
use futures::StreamExt;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    GenericNamespaced, ListenerOptions, Name,
};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tarpc::context;
use tarpc::serde_transport as transport;
use tarpc::server::{BaseChannel, Channel};
use tarpc::tokio_serde::formats::Bincode;
use tarpc::tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub ipc_name: String,
    pub model_path: Option<PathBuf>,
    pub queue_capacity: usize,
    pub embedding_threads: usize,
    pub limits: WorkerLimits,
}

impl WorkerConfig {
    pub fn new(ipc_name: String) -> anyhow::Result<Self> {
        Ok(Self {
            ipc_name,
            model_path: std::env::var("NOTE_MODEL_PATH")
                .ok()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            queue_capacity: env_usize("NOTE_EMBEDDING_WORKER_QUEUE", 8),
            embedding_threads: embedding_threads_from_env()?,
            limits: WorkerLimits::default(),
        })
    }
}

#[derive(Clone)]
struct WorkerService {
    model: ModelInfo,
    limits: WorkerLimits,
    queue: mpsc::Sender<WorkItem>,
    in_flight: Arc<AtomicUsize>,
    shutting_down: Arc<AtomicBool>,
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

struct WorkItem {
    request: EmbedRequest,
    reply: oneshot::Sender<Result<EmbedResponse, RpcError>>,
}

impl EmbeddingRpc for WorkerService {
    async fn handshake(
        self,
        _: context::Context,
        request: HandshakeRequest,
    ) -> Result<HandshakeResponse, RpcError> {
        validate_handshake(&request, &self.model)?;
        Ok(HandshakeResponse {
            protocol_version: crate::rpc::IPC_PROTOCOL_VERSION,
            model: self.model,
            limits: self.limits,
        })
    }

    async fn health(self, _: context::Context) -> Result<HealthResponse, RpcError> {
        Ok(HealthResponse {
            ready: !self.shutting_down.load(Ordering::SeqCst),
            in_flight: self.in_flight.load(Ordering::SeqCst),
            queued_capacity: self.queue.capacity(),
        })
    }

    async fn model_info(self, _: context::Context) -> Result<ModelInfo, RpcError> {
        Ok(self.model)
    }

    async fn embed(
        self,
        ctx: context::Context,
        request: EmbedRequest,
    ) -> Result<EmbedResponse, RpcError> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RpcError::new(
                RpcErrorKind::ShuttingDown,
                "worker is shutting down",
            ));
        }
        validate_embed_request(&request, &self.limits)?;

        let (reply, rx) = oneshot::channel();
        let item = WorkItem { request, reply };
        self.queue.try_send(item).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                RpcError::new(RpcErrorKind::Capacity, "embedding worker queue is full")
            }
            mpsc::error::TrySendError::Closed(_) => RpcError::new(
                RpcErrorKind::ShuttingDown,
                "embedding worker queue is closed",
            ),
        })?;

        let deadline = tokio::time::Instant::from_std(ctx.deadline);
        let response = tokio::time::timeout_at(deadline, rx)
            .await
            .map_err(|_| RpcError::new(RpcErrorKind::Timeout, "embedding request timed out"))?
            .map_err(|_| RpcError::new(RpcErrorKind::ShuttingDown, "embedding worker stopped"))??;

        maybe_crash_once_after_response();
        Ok(response)
    }

    async fn shutdown(self, _: context::Context) -> Result<(), RpcError> {
        self.shutting_down.store(true, Ordering::SeqCst);
        if let Some(tx) = self
            .shutdown
            .lock()
            .expect("shutdown mutex poisoned")
            .take()
        {
            let _ = tx.send(());
        }
        Ok(())
    }
}

pub async fn run_embedding_worker(config: WorkerConfig) -> anyhow::Result<()> {
    let socket_name = local_socket_name(&config.ipc_name)?;
    let (embedder, model) =
        build_local_embedder(config.model_path.as_deref(), 1, config.embedding_threads)?;
    let (tx, rx) = mpsc::channel(config.queue_capacity.max(1));
    let in_flight = Arc::new(AtomicUsize::new(0));
    tokio::spawn(embed_actor(
        embedder,
        rx,
        config.limits.clone(),
        in_flight.clone(),
    ));

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let service = WorkerService {
        model,
        limits: config.limits,
        queue: tx,
        in_flight,
        shutting_down: Arc::new(AtomicBool::new(false)),
        shutdown: Arc::new(Mutex::new(Some(shutdown_tx))),
    };

    let listener = ListenerOptions::new()
        .name(socket_name.borrow())
        .try_overwrite(true)
        .reclaim_name(true)
        .create_tokio()?;
    eprintln!("embedding worker ready: {}", config.ipc_name);

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let conn = match accepted {
                    Ok(conn) => conn,
                    Err(error) => {
                        eprintln!("embedding worker accept error: {error}");
                        continue;
                    }
                };
                let framed = LengthDelimitedCodec::builder().new_framed(conn);
                let transport = transport::new(framed, Bincode::default());
                let fut = BaseChannel::with_defaults(transport)
                    .execute(service.clone().serve())
                    .for_each(spawn_response);
                tokio::spawn(fut);
            }
        }
    }

    Ok(())
}

pub async fn connect_client(ipc_name: &str) -> anyhow::Result<EmbeddingRpcClient> {
    let socket_name = local_socket_name(ipc_name)?;
    let stream = Stream::connect(socket_name.borrow()).await?;
    let framed = LengthDelimitedCodec::builder().new_framed(stream);
    let transport = transport::new(framed, Bincode::default());
    Ok(EmbeddingRpcClient::new(tarpc::client::Config::default(), transport).spawn())
}

pub fn local_socket_name(raw: &str) -> std::io::Result<Name<'static>> {
    raw.to_ns_name::<GenericNamespaced>().map(Name::into_owned)
}

pub fn build_local_embedder(
    model_path: Option<&Path>,
    concurrency: usize,
    embedding_threads: usize,
) -> anyhow::Result<(Arc<dyn Embedder>, ModelInfo)> {
    match model_path {
        Some(path) => {
            eprintln!(
                "embedder: ONNX ({}) with {} CPU threads",
                path.display(),
                embedding_threads
            );
            let ort = OrtEmbedder::load(path, embedding_threads)?;
            Ok((
                Arc::new(BoundedEmbedder::new(ort, concurrency.max(1))),
                ModelInfo::onnx(&path.display().to_string()),
            ))
        }
        None => {
            eprintln!("embedder: stub");
            Ok((
                Arc::new(BoundedEmbedder::new(StubEmbedder, concurrency.max(1))),
                ModelInfo::stub(),
            ))
        }
    }
}

async fn embed_actor(
    embedder: Arc<dyn Embedder>,
    mut rx: mpsc::Receiver<WorkItem>,
    limits: WorkerLimits,
    in_flight: Arc<AtomicUsize>,
) {
    while let Some(item) = rx.recv().await {
        in_flight.fetch_add(1, Ordering::SeqCst);
        let response = process_embed_request(embedder.as_ref(), item.request, &limits).await;
        in_flight.fetch_sub(1, Ordering::SeqCst);
        let _ = item.reply.send(response);
    }
}

async fn process_embed_request(
    embedder: &dyn Embedder,
    request: EmbedRequest,
    limits: &WorkerLimits,
) -> Result<EmbedResponse, RpcError> {
    let texts = request
        .inputs
        .iter()
        .map(|input| input.text.clone())
        .collect::<Vec<_>>();
    let embedded = embedder.embed_batch(&texts).await.map_err(|error| {
        RpcError::new(
            RpcErrorKind::Inference,
            format!("embedding failed: {error:#}"),
        )
    })?;

    let outputs = request
        .inputs
        .into_iter()
        .zip(embedded)
        .map(|(input, dense)| crate::rpc::EmbedOutput {
            job_id: input.job_id,
            result: Ok(dense),
        })
        .collect::<Vec<_>>();
    let response = EmbedResponse {
        request_id: request.request_id,
        outputs,
    };
    let requested_max = request.max_response_bytes.min(limits.max_response_bytes);
    let max_response_bytes = if requested_max == 0 {
        limits.max_response_bytes
    } else {
        requested_max
    };
    if estimate_response_bytes(&response) > max_response_bytes {
        return Err(RpcError::new(
            RpcErrorKind::ResponseTooLarge,
            "embedding response exceeded response size limit",
        ));
    }
    Ok(response)
}

fn validate_embed_request(request: &EmbedRequest, limits: &WorkerLimits) -> Result<(), RpcError> {
    if request.inputs.is_empty() {
        return Err(RpcError::new(
            RpcErrorKind::InvalidRequest,
            "embedding request has no inputs",
        ));
    }
    if request.inputs.len() > limits.max_batch_inputs {
        return Err(RpcError::new(
            RpcErrorKind::InvalidRequest,
            format!(
                "embedding request has {} inputs, limit is {}",
                request.inputs.len(),
                limits.max_batch_inputs
            ),
        ));
    }
    let input_bytes: usize = request.inputs.iter().map(|input| input.text.len()).sum();
    if input_bytes > limits.max_input_bytes {
        return Err(RpcError::new(
            RpcErrorKind::InvalidRequest,
            format!("embedding request has {input_bytes} bytes"),
        ));
    }
    if request
        .deadline_unix_ms
        .is_some_and(|deadline| deadline <= unix_time_ms())
    {
        return Err(RpcError::new(
            RpcErrorKind::Timeout,
            "embedding request deadline has passed",
        ));
    }
    Ok(())
}

fn estimate_response_bytes(response: &EmbedResponse) -> usize {
    response
        .outputs
        .iter()
        .map(|output| match &output.result {
            Ok(dense) => dense.len() * std::mem::size_of::<f32>() + 32,
            Err(error) => error.message.len() + 32,
        })
        .sum::<usize>()
        + response.request_id.len()
}

async fn spawn_response(fut: impl futures::Future<Output = ()> + Send + 'static) {
    tokio::spawn(fut);
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub(crate) fn embedding_threads_from_env() -> anyhow::Result<usize> {
    let available_cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    resolve_embedding_threads(
        std::env::var("NOTE_EMBEDDING_THREADS").ok().as_deref(),
        num_cpus::get_physical(),
        available_cores,
    )
}

fn resolve_embedding_threads(
    value: Option<&str>,
    physical_cores: usize,
    available_cores: usize,
) -> anyhow::Result<usize> {
    let auto = physical_cores.min(available_cores).max(1);
    match value.map(str::trim) {
        None | Some("") | Some("auto") => Ok(auto),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "NOTE_EMBEDDING_THREADS must be 'auto' or a positive integer, got {value:?}"
                )
            }),
    }
}

fn maybe_crash_once_after_response() {
    let Some(path) = std::env::var_os("NOTE_EMBEDDING_WORKER_CRASH_ONCE_FILE") else {
        return;
    };
    let path = PathBuf::from(path);
    if path.exists() {
        return;
    }
    let _ = std::fs::write(&path, b"crashed");
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::process::exit(86);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{EmbedInput, DEFAULT_MAX_RESPONSE_BYTES};

    #[tokio::test]
    async fn rejects_oversized_batch() {
        let request = EmbedRequest {
            request_id: "req".to_string(),
            deadline_unix_ms: None,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            inputs: (0..DEFAULT_MAX_RESPONSE_BYTES.min(32))
                .map(|idx| EmbedInput {
                    job_id: idx as i64,
                    text: "x".to_string(),
                })
                .collect(),
        };
        let limits = WorkerLimits {
            max_batch_inputs: 1,
            ..WorkerLimits::default()
        };
        let error = validate_embed_request(&request, &limits).unwrap_err();
        assert_eq!(error.kind, RpcErrorKind::InvalidRequest);
    }

    #[test]
    fn response_size_counts_dense_payload_only() {
        let response = EmbedResponse {
            request_id: "req".to_string(),
            outputs: vec![crate::rpc::EmbedOutput {
                job_id: 1,
                result: Ok(vec![0.0; 4]),
            }],
        };
        assert_eq!(
            estimate_response_bytes(&response),
            "req".len() + 4 * std::mem::size_of::<f32>() + 32
        );
    }

    #[test]
    fn embedding_threads_default_and_auto_use_physical_cores() {
        assert_eq!(resolve_embedding_threads(None, 16, 32).unwrap(), 16);
        assert_eq!(resolve_embedding_threads(Some("auto"), 16, 32).unwrap(), 16);
        assert_eq!(resolve_embedding_threads(Some(""), 16, 32).unwrap(), 16);
        assert_eq!(
            resolve_embedding_threads(Some(" auto "), 16, 32).unwrap(),
            16
        );
        assert_eq!(resolve_embedding_threads(None, 0, 32).unwrap(), 1);
    }

    #[test]
    fn embedding_threads_auto_respects_available_cpus() {
        assert_eq!(resolve_embedding_threads(None, 16, 4).unwrap(), 4);
        assert_eq!(resolve_embedding_threads(Some("auto"), 16, 0).unwrap(), 1);
    }

    #[test]
    fn embedding_threads_accept_positive_override() {
        assert_eq!(resolve_embedding_threads(Some("1"), 16, 32).unwrap(), 1);
        assert_eq!(resolve_embedding_threads(Some(" 6 "), 16, 32).unwrap(), 6);
    }

    #[test]
    fn embedding_threads_reject_invalid_override() {
        for value in ["0", "-1", "many"] {
            let error = resolve_embedding_threads(Some(value), 16, 32).unwrap_err();
            assert!(error.to_string().contains("NOTE_EMBEDDING_THREADS"));
        }
    }
}
