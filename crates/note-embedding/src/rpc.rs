use crate::DenseVector;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const IPC_PROTOCOL_VERSION: u16 = 2;
pub const DEFAULT_EMBEDDING_DIMENSION: usize = 1024;
pub const DEFAULT_MAX_BATCH_INPUTS: usize = 16;
pub const DEFAULT_MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
pub const CAPABILITY_DENSE: &str = "dense";
pub const CAPABILITY_BATCH: &str = "batch";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub model_id: String,
    pub model_version: String,
    pub embedding_dimension: usize,
    pub capabilities: Vec<String>,
}

impl ModelInfo {
    pub fn stub() -> Self {
        Self {
            model_id: "stub".to_string(),
            model_version: "deterministic-v1".to_string(),
            embedding_dimension: DEFAULT_EMBEDDING_DIMENSION,
            capabilities: default_capabilities(),
        }
    }

    pub fn onnx(model_path: &str) -> Self {
        Self {
            model_id: model_path.to_string(),
            model_version: "onnx-runtime".to_string(),
            embedding_dimension: DEFAULT_EMBEDDING_DIMENSION,
            capabilities: default_capabilities(),
        }
    }
}

pub fn default_capabilities() -> Vec<String> {
    [CAPABILITY_DENSE, CAPABILITY_BATCH]
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerLimits {
    pub max_batch_inputs: usize,
    pub max_input_bytes: usize,
    pub max_response_bytes: usize,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            max_batch_inputs: DEFAULT_MAX_BATCH_INPUTS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandshakeRequest {
    pub protocol_version: u16,
    pub expected_model_id: Option<String>,
    pub expected_dimension: usize,
    pub required_capabilities: Vec<String>,
}

impl Default for HandshakeRequest {
    fn default() -> Self {
        Self {
            protocol_version: IPC_PROTOCOL_VERSION,
            expected_model_id: None,
            expected_dimension: DEFAULT_EMBEDDING_DIMENSION,
            required_capabilities: default_capabilities(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandshakeResponse {
    pub protocol_version: u16,
    pub model: ModelInfo,
    pub limits: WorkerLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub ready: bool,
    pub in_flight: usize,
    pub queued_capacity: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedRequest {
    pub request_id: String,
    pub deadline_unix_ms: Option<u64>,
    pub max_response_bytes: usize,
    pub inputs: Vec<EmbedInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedInput {
    pub job_id: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedResponse {
    pub request_id: String,
    pub outputs: Vec<EmbedOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedOutput {
    pub job_id: i64,
    pub result: Result<DenseVector, RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcError {
    pub kind: RpcErrorKind,
    pub message: String,
}

impl RpcError {
    pub fn new(kind: RpcErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RpcErrorKind {
    Capacity,
    InvalidRequest,
    ProtocolMismatch,
    ModelMismatch,
    Timeout,
    Inference,
    Transport,
    ShuttingDown,
    ResponseTooLarge,
}

pub fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn validate_handshake(request: &HandshakeRequest, model: &ModelInfo) -> Result<(), RpcError> {
    if request.protocol_version != IPC_PROTOCOL_VERSION {
        return Err(RpcError::new(
            RpcErrorKind::ProtocolMismatch,
            format!(
                "protocol version {} is not supported by worker version {}",
                request.protocol_version, IPC_PROTOCOL_VERSION
            ),
        ));
    }
    if let Some(expected) = &request.expected_model_id {
        if expected != &model.model_id {
            return Err(RpcError::new(
                RpcErrorKind::ModelMismatch,
                format!("worker model {} does not match {expected}", model.model_id),
            ));
        }
    }
    if request.expected_dimension != model.embedding_dimension {
        return Err(RpcError::new(
            RpcErrorKind::ModelMismatch,
            format!(
                "worker dimension {} does not match {}",
                model.embedding_dimension, request.expected_dimension
            ),
        ));
    }
    for capability in &request.required_capabilities {
        if !model.capabilities.iter().any(|item| item == capability) {
            return Err(RpcError::new(
                RpcErrorKind::ModelMismatch,
                format!("worker is missing capability {capability}"),
            ));
        }
    }
    Ok(())
}

#[tarpc::service]
pub trait EmbeddingRpc {
    async fn handshake(request: HandshakeRequest) -> Result<HandshakeResponse, RpcError>;
    async fn health() -> Result<HealthResponse, RpcError>;
    async fn model_info() -> Result<ModelInfo, RpcError>;
    async fn embed(request: EmbedRequest) -> Result<EmbedResponse, RpcError>;
    async fn shutdown() -> Result<(), RpcError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_capabilities_are_dense_and_batch_only() {
        assert_eq!(
            default_capabilities(),
            vec![CAPABILITY_DENSE.to_string(), CAPABILITY_BATCH.to_string()]
        );
    }

    #[test]
    fn rpc_messages_round_trip_through_bincode() {
        let request = EmbedRequest {
            request_id: "req-1".to_string(),
            deadline_unix_ms: Some(123),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            inputs: vec![EmbedInput {
                job_id: 7,
                text: "hello".to_string(),
            }],
        };

        let bytes = bincode::serialize(&request).unwrap();
        assert!(bytes.len() < 256);
        let decoded: EmbedRequest = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn dense_response_round_trips_through_bincode() {
        let response = EmbedResponse {
            request_id: "req-1".to_string(),
            outputs: vec![EmbedOutput {
                job_id: 7,
                result: Ok(vec![0.25; DEFAULT_EMBEDDING_DIMENSION]),
            }],
        };
        let bytes = bincode::serialize(&response).unwrap();
        let decoded: EmbedResponse = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn handshake_rejects_protocol_mismatch() {
        let request = HandshakeRequest {
            protocol_version: IPC_PROTOCOL_VERSION + 1,
            ..HandshakeRequest::default()
        };
        let error = validate_handshake(&request, &ModelInfo::stub()).unwrap_err();
        assert_eq!(error.kind, RpcErrorKind::ProtocolMismatch);
    }
}
