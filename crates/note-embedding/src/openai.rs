use crate::{DenseVector, Embedder, DEFAULT_EMBEDDING_DIMENSION};
use anyhow::{anyhow, bail, Result};
use futures::StreamExt;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::{Client, Response, StatusCode, Url};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_RETRIES: u32 = 10;
const MAX_ERROR_BODY_BYTES: usize = 4_096;
// The default RPC batch is 16 BGE-M3 vectors (16,384 JSON floats). One MiB leaves ample
// serialization overhead while keeping an untrusted compatible endpoint strictly bounded.
const MAX_SUCCESS_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuccessBodyReadError {
    TooLarge,
    ReadFailed,
}

#[derive(Clone)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub model: String,
    pub bearer_token: Option<String>,
    pub timeout: Duration,
    pub max_retries: u32,
}

pub struct OpenAiCompatibleEmbedder {
    client: Client,
    endpoint: Url,
    model: String,
    authorization: Option<HeaderValue>,
    max_retries: u32,
}

#[derive(serde::Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    encoding_format: &'static str,
}

#[derive(serde::Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingResponseItem>,
}

#[derive(serde::Deserialize)]
struct EmbeddingResponseItem {
    index: usize,
    embedding: Vec<f32>,
}

impl OpenAiCompatibleEmbedder {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self> {
        let mut endpoint =
            Url::parse(&config.base_url).map_err(|_| anyhow!("embedding base URL is invalid"))?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            bail!("embedding base URL scheme must be http or https");
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            bail!("embedding base URL credentials are not allowed");
        }
        if endpoint.query().is_some() {
            bail!("embedding base URL query is not allowed");
        }
        if endpoint.fragment().is_some() {
            bail!("embedding base URL fragment is not allowed");
        }
        {
            let mut segments = endpoint
                .path_segments_mut()
                .map_err(|_| anyhow!("embedding base URL cannot be used as a base URL"))?;
            segments.pop_if_empty().push("v1").push("embeddings");
        }

        let model = config.model.trim().to_owned();
        if model.is_empty() {
            bail!("embedding model is blank");
        }
        if config.max_retries > MAX_RETRIES {
            bail!("embedding max_retries must be at most {MAX_RETRIES}");
        }

        let authorization = config
            .bearer_token
            .map(|token| {
                let value = format!("Bearer {token}");
                let mut header = HeaderValue::from_str(&value)
                    .map_err(|_| anyhow!("embedding bearer token is invalid"))?;
                header.set_sensitive(true);
                Ok::<HeaderValue, anyhow::Error>(header)
            })
            .transpose()?;

        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|_| anyhow!("embedding HTTP client configuration is invalid"))?;

        Ok(Self {
            client,
            endpoint,
            model,
            authorization,
            max_retries: config.max_retries,
        })
    }

    async fn send_batch(&self, texts: &[String]) -> Result<Vec<DenseVector>> {
        for attempt in 0..=self.max_retries {
            let request = EmbeddingRequest {
                model: &self.model,
                input: texts,
                encoding_format: "float",
            };
            let mut builder = self.client.post(self.endpoint.clone()).json(&request);
            if let Some(authorization) = &self.authorization {
                builder = builder.header(AUTHORIZATION, authorization.clone());
            }

            match builder.send().await {
                Ok(response) if response.status().is_success() => {
                    let response = parse_success_response(response).await?;
                    return validate_response_items(texts.len(), response.data);
                }
                Ok(response) => {
                    if retryable_status(response.status()) && attempt < self.max_retries {
                        drop(response);
                        sleep_before_retry(attempt.saturating_add(1)).await;
                        continue;
                    }
                    return Err(http_error(response, self.authorization.is_some()).await);
                }
                Err(error) => {
                    if retryable_transport(&error) && attempt < self.max_retries {
                        sleep_before_retry(attempt.saturating_add(1)).await;
                        continue;
                    }
                    return Err(transport_error(&error));
                }
            }
        }

        unreachable!("inclusive attempt loop always returns on its final iteration")
    }
}

#[async_trait::async_trait]
impl Embedder for OpenAiCompatibleEmbedder {
    async fn embed(&self, text: &str) -> Result<DenseVector> {
        let outputs = self.embed_batch(&[text.to_owned()]).await?;
        outputs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("embedding response was empty"))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<DenseVector>> {
        if texts.is_empty() {
            bail!("embedding input batch is empty");
        }
        for (index, text) in texts.iter().enumerate() {
            if text.trim().is_empty() {
                bail!("embedding input {index} is blank");
            }
        }
        self.send_batch(texts).await
    }
}

fn validate_response_items(
    expected: usize,
    items: Vec<EmbeddingResponseItem>,
) -> Result<Vec<DenseVector>> {
    if items.len() != expected {
        bail!(
            "embedding response expected {expected} items, got {}",
            items.len()
        );
    }

    let mut ordered = vec![None; expected];
    for item in items {
        if item.index >= expected {
            bail!(
                "embedding response index {} out of range 0..{expected}",
                item.index
            );
        }
        if ordered[item.index].is_some() {
            bail!("embedding response duplicate response index {}", item.index);
        }
        if item.embedding.len() != DEFAULT_EMBEDDING_DIMENSION {
            bail!(
                "embedding response expected {DEFAULT_EMBEDDING_DIMENSION} components, got {}",
                item.embedding.len()
            );
        }
        if item
            .embedding
            .iter()
            .any(|component| !component.is_finite())
        {
            bail!("embedding response contains a non-finite component");
        }
        ordered[item.index] = Some(item.embedding);
    }

    ordered
        .into_iter()
        .map(|slot| slot.ok_or_else(|| anyhow!("embedding response omitted an index")))
        .collect()
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retryable_transport(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout()
}

fn transport_error(error: &reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow!("embedding request timed out")
    } else if error.is_connect() {
        anyhow!("embedding request connection failed")
    } else {
        anyhow!("embedding request failed")
    }
}

async fn parse_success_response(response: Response) -> Result<EmbeddingResponse> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SUCCESS_BODY_BYTES as u64)
    {
        bail!("embedding response exceeds maximum size");
    }

    let body =
        match read_bounded_success_stream(response.bytes_stream(), MAX_SUCCESS_BODY_BYTES).await {
            Ok(body) => body,
            Err(SuccessBodyReadError::TooLarge) => {
                bail!("embedding response exceeds maximum size")
            }
            Err(SuccessBodyReadError::ReadFailed) => {
                bail!("embedding response could not be read")
            }
        };
    serde_json::from_slice(&body).map_err(|_| anyhow!("embedding response was not valid JSON"))
}

async fn read_bounded_success_stream<S, T, E>(
    stream: S,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, SuccessBodyReadError>
where
    S: futures::Stream<Item = std::result::Result<T, E>>,
    T: AsRef<[u8]>,
{
    futures::pin_mut!(stream);
    let mut body = Vec::with_capacity(max_bytes.min(64 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| SuccessBodyReadError::ReadFailed)?;
        let chunk = chunk.as_ref();
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            return Err(SuccessBodyReadError::TooLarge);
        }
        body.extend_from_slice(chunk);
    }
    Ok(body)
}

async fn http_error(response: Response, authenticated: bool) -> anyhow::Error {
    let status = response.status();
    if authenticated {
        return anyhow!("embedding request failed with HTTP {status}; response body [REDACTED]");
    }

    match bounded_response_body(response).await {
        Ok(body) => anyhow!("embedding request failed with HTTP {status}; response body {body}"),
        Err(()) => anyhow!("embedding request failed with HTTP {status}; response body unreadable"),
    }
}

async fn bounded_response_body(response: Response) -> Result<String, ()> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::with_capacity(MAX_ERROR_BODY_BYTES);
    let mut truncated = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ())?;
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }

    let mut text = String::from_utf8_lossy(&body).into_owned();
    if truncated {
        text.push_str("...[truncated]");
    }
    Ok(text)
}

async fn sleep_before_retry(retry_number: u32) {
    let exponent = retry_number.saturating_sub(1).min(31);
    let factor = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    let lower = 50_u64.saturating_mul(factor).min(2_000);
    let upper = 100_u64.saturating_mul(factor).min(2_000);
    let width = upper.saturating_sub(lower).saturating_add(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let delay = lower.saturating_add(nanos % width);
    tokio::time::sleep(Duration::from_millis(delay)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_finite_components_before_returning_vectors() {
        let error = validate_response_items(
            1,
            vec![EmbeddingResponseItem {
                index: 0,
                embedding: vec![f32::NAN; DEFAULT_EMBEDDING_DIMENSION],
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("non-finite component"));
    }

    #[tokio::test]
    async fn bounded_success_stream_rejects_chunked_body_past_limit() {
        let stream = futures::stream::iter([
            Ok::<_, ()>(b"1234".to_vec()),
            Ok(b"5678".to_vec()),
            Ok(b"9".to_vec()),
        ]);

        let error = read_bounded_success_stream(stream, 8).await.unwrap_err();

        assert_eq!(error, SuccessBodyReadError::TooLarge);
    }

    #[tokio::test]
    async fn bounded_success_stream_allows_body_at_exact_limit() {
        let stream = futures::stream::iter([Ok::<_, ()>(b"1234".to_vec()), Ok(b"5678".to_vec())]);

        let body = read_bounded_success_stream(stream, 8).await.unwrap();

        assert_eq!(body, b"12345678");
    }
}
