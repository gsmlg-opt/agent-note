use note_embedding::{
    embedding_fingerprint, Embedder, EmbeddingBackendInfo, OpenAiCompatibleConfig,
    OpenAiCompatibleEmbedder, DEFAULT_BGE_M3_MODEL, DEFAULT_EMBEDDING_DIMENSION,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn vector(value: f32) -> Vec<f32> {
    vec![value; DEFAULT_EMBEDDING_DIMENSION]
}

fn config(server: &MockServer) -> OpenAiCompatibleConfig {
    OpenAiCompatibleConfig {
        base_url: server.uri(),
        model: DEFAULT_BGE_M3_MODEL.into(),
        bearer_token: None,
        timeout: Duration::from_secs(1),
        max_retries: 0,
    }
}

fn response(index: usize, value: f32) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "data": [{"index": index, "embedding": vector(value)}]
    }))
}

#[tokio::test]
async fn unauthenticated_batch_uses_openai_shape_without_dimensions() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_json(serde_json::json!({
            "model": "bge-m3",
            "input": ["first", "second"],
            "encoding_format": "float"
        })))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"index": 0, "embedding": vector(0.1)},
                {"index": 1, "embedding": vector(0.2)}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let adapter = OpenAiCompatibleEmbedder::new(config(&server)).unwrap();
    let output = adapter
        .embed_batch(&["first".into(), "second".into()])
        .await
        .unwrap();

    assert_eq!(output, vec![vector(0.1), vector(0.2)]);
    let requests = server.received_requests().await.unwrap();
    assert!(!requests[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn optional_bearer_auth_and_trimmed_configured_model_are_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer secret-token"))
        .and(body_json(serde_json::json!({
            "model": "private-bge-m3",
            "input": ["private"],
            "encoding_format": "float"
        })))
        .respond_with(response(0, 0.3))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.model = "  private-bge-m3  ".into();
    cfg.bearer_token = Some("secret-token".into());

    let output = OpenAiCompatibleEmbedder::new(cfg)
        .unwrap()
        .embed("private")
        .await
        .unwrap();

    assert_eq!(output, vector(0.3));
}

#[tokio::test]
async fn response_vectors_are_reordered_by_index() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"index": 1, "embedding": vector(0.2)},
                {"index": 0, "embedding": vector(0.1)}
            ]
        })))
        .mount(&server)
        .await;

    let output = OpenAiCompatibleEmbedder::new(config(&server))
        .unwrap()
        .embed_batch(&["first".into(), "second".into()])
        .await
        .unwrap();

    assert_eq!(output, vec![vector(0.1), vector(0.2)]);
}

#[tokio::test]
async fn response_count_must_match_input_count() {
    let error = response_validation_error(
        &["first", "second"],
        serde_json::json!({"data": [{"index": 0, "embedding": vector(0.1)}]}),
    )
    .await;
    assert!(error.contains("expected 2 items, got 1"), "{error}");
}

#[tokio::test]
async fn response_indices_must_be_unique() {
    let error = response_validation_error(
        &["first", "second"],
        serde_json::json!({
            "data": [
                {"index": 0, "embedding": vector(0.1)},
                {"index": 0, "embedding": vector(0.2)}
            ]
        }),
    )
    .await;
    assert!(error.contains("duplicate response index 0"), "{error}");
}

#[tokio::test]
async fn response_indices_must_be_in_range() {
    let error = response_validation_error(
        &["first", "second"],
        serde_json::json!({
            "data": [
                {"index": 0, "embedding": vector(0.1)},
                {"index": 2, "embedding": vector(0.2)}
            ]
        }),
    )
    .await;
    assert!(
        error.contains("response index 2 out of range 0..2"),
        "{error}"
    );
}

#[tokio::test]
async fn response_vectors_must_have_bge_m3_dimension() {
    let error = response_validation_error(
        &["first"],
        serde_json::json!({"data": [{"index": 0, "embedding": vec![0.1; 1023]}]}),
    )
    .await;
    assert!(
        error.contains("expected 1024 components, got 1023"),
        "{error}"
    );
}

async fn response_validation_error(inputs: &[&str], body: serde_json::Value) -> String {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    OpenAiCompatibleEmbedder::new(config(&server))
        .unwrap()
        .embed_batch(
            &inputs
                .iter()
                .map(|input| (*input).to_owned())
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap_err()
        .to_string()
}

#[tokio::test]
async fn empty_batch_is_rejected_without_an_http_request() {
    let server = MockServer::start().await;
    let error = OpenAiCompatibleEmbedder::new(config(&server))
        .unwrap()
        .embed_batch(&[])
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("embedding input batch is empty"),
        "{error}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn blank_input_is_rejected_with_its_index_without_an_http_request() {
    let server = MockServer::start().await;
    let error = OpenAiCompatibleEmbedder::new(config(&server))
        .unwrap()
        .embed_batch(&["first".into(), " \n\t ".into()])
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("embedding input 1 is blank"),
        "{error}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn retries_5xx_then_returns_success() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = calls.clone();
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |_: &wiremock::Request| {
            if responder_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503).set_body_string("temporarily unavailable")
            } else {
                response(0, 0.4)
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.max_retries = 2;

    let output = OpenAiCompatibleEmbedder::new(cfg)
        .unwrap()
        .embed("retry me")
        .await
        .unwrap();

    assert_eq!(output, vector(0.4));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_429_then_returns_success() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = calls.clone();
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            if responder_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429)
            } else {
                response(0, 0.5)
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.max_retries = 1;

    let output = OpenAiCompatibleEmbedder::new(cfg)
        .unwrap()
        .embed("rate limited")
        .await
        .unwrap();

    assert_eq!(output, vector(0.5));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn does_not_retry_http_400() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("invalid request"))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.max_retries = 3;

    let error = OpenAiCompatibleEmbedder::new(cfg)
        .unwrap()
        .embed("bad")
        .await
        .unwrap_err();

    assert!(error.to_string().contains("400"), "{error}");
}

#[tokio::test]
async fn timeout_is_retried_exactly_max_retries_plus_one_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(serde_json::json!({
                    "data": [{"index": 0, "embedding": vector(0.1)}]
                })),
        )
        .expect(3)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.timeout = Duration::from_millis(20);
    cfg.max_retries = 2;

    let error = OpenAiCompatibleEmbedder::new(cfg)
        .unwrap()
        .embed("slow")
        .await
        .unwrap_err();

    assert!(error.to_string().contains("timed out"), "{error}");
}

#[tokio::test]
async fn authenticated_http_error_redacts_the_entire_response_body() {
    let server = MockServer::start().await;
    let leaked_body = format!("secret-token secret-{}", "x".repeat(8_000));
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(leaked_body))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.bearer_token = Some("secret-token".into());

    let error = OpenAiCompatibleEmbedder::new(cfg)
        .unwrap()
        .embed("private")
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("response body [REDACTED]"), "{error}");
    assert!(!error.contains("secret-token"), "{error}");
    assert!(!error.contains("secret-"), "{error}");
    assert!(error.len() < 5_000, "error was {} bytes", error.len());
}

#[tokio::test]
async fn unauthenticated_http_error_body_is_bounded_and_marked_truncated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("x".repeat(8_000)))
        .mount(&server)
        .await;

    let error = OpenAiCompatibleEmbedder::new(config(&server))
        .unwrap()
        .embed("bad")
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("...[truncated]"), "{error}");
    assert!(error.len() < 5_000, "error was {} bytes", error.len());
}

#[tokio::test]
async fn base_path_and_trailing_slash_are_preserved_when_appending_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/gateway/api/v1/embeddings"))
        .respond_with(response(0, 0.7))
        .expect(2)
        .mount(&server)
        .await;

    for suffix in ["/gateway/api", "/gateway/api/"] {
        let mut cfg = config(&server);
        cfg.base_url = format!("{}{suffix}", server.uri());
        let output = OpenAiCompatibleEmbedder::new(cfg)
            .unwrap()
            .embed("path")
            .await
            .unwrap();
        assert_eq!(output, vector(0.7));
    }
}

#[test]
fn constructor_rejects_invalid_or_unsafe_urls_without_echoing_them() {
    for (base_url, category) in [
        ("not a url secret-input", "invalid"),
        ("https://user@example.com", "credentials"),
        ("https://user:secret@example.com", "credentials"),
        ("https://example.com?secret=query", "query"),
        ("https://example.com#secret-fragment", "fragment"),
    ] {
        let mut cfg = standalone_config(base_url);
        cfg.bearer_token = Some("header-secret".into());
        let error = OpenAiCompatibleEmbedder::new(cfg)
            .err()
            .expect("configuration should be rejected")
            .to_string();
        assert!(error.contains(category), "{error}");
        assert!(!error.contains("secret"), "{error}");
        assert!(!error.contains("header-secret"), "{error}");
    }
}

#[test]
fn constructor_rejects_blank_model_invalid_token_and_excessive_retries_safely() {
    let mut blank = standalone_config("https://example.com");
    blank.model = " \t".into();
    assert!(OpenAiCompatibleEmbedder::new(blank)
        .err()
        .unwrap()
        .to_string()
        .contains("model is blank"));

    let mut retries = standalone_config("https://example.com");
    retries.max_retries = 11;
    assert!(OpenAiCompatibleEmbedder::new(retries)
        .err()
        .unwrap()
        .to_string()
        .contains("max_retries must be at most 10"));

    let mut token = standalone_config("https://example.com");
    token.bearer_token = Some("secret-token\ninvalid".into());
    let error = OpenAiCompatibleEmbedder::new(token)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("bearer token is invalid"), "{error}");
    assert!(!error.contains("secret-token"), "{error}");
}

fn standalone_config(base_url: &str) -> OpenAiCompatibleConfig {
    OpenAiCompatibleConfig {
        base_url: base_url.into(),
        model: DEFAULT_BGE_M3_MODEL.into(),
        bearer_token: None,
        timeout: Duration::from_secs(1),
        max_retries: 0,
    }
}

#[test]
fn embedding_backend_metadata_uses_trimmed_model_and_stable_fingerprint() {
    assert_eq!(
        EmbeddingBackendInfo::local_bge_m3(),
        EmbeddingBackendInfo {
            engine: "local".into(),
            model: "bge-m3".into(),
            fingerprint: "bge-m3:1024".into(),
        }
    );
    assert_eq!(
        EmbeddingBackendInfo::openai("  private-bge-m3  "),
        EmbeddingBackendInfo {
            engine: "openai".into(),
            model: "private-bge-m3".into(),
            fingerprint: "private-bge-m3:1024".into(),
        }
    );
    assert_eq!(embedding_fingerprint(" bge-m3 "), "bge-m3:1024");
}
