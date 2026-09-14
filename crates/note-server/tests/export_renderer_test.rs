use note_server::export::document::{ExportDocumentPackage, PackagedExportAsset};
use note_server::export::renderer::{
    max_renderer_package_bytes, GotenbergRenderer, PdfRenderer, PdfRendererError,
};
use std::convert::Infallible;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn package() -> ExportDocumentPackage {
    ExportDocumentPackage {
        index_html: "<!doctype html><p>hello</p>".into(),
        footer_html: "<!doctype html><p>footer</p>".into(),
        assets: vec![PackagedExportAsset {
            filename: "asset-0001.png".into(),
            mime: "image/png".into(),
            bytes: vec![1, 2, 3],
        }],
    }
}

#[test]
fn renderer_package_budget_keeps_asset_and_html_limits_independent() {
    let max_html = 32;
    let max_footer = 16;
    let max_assets = 48;
    let max_package = max_renderer_package_bytes(max_html, max_footer, max_assets, 2).unwrap();
    let package = ExportDocumentPackage {
        index_html: "h".repeat(max_html),
        footer_html: "f".repeat(max_footer),
        assets: vec![
            PackagedExportAsset {
                filename: "a".repeat(180),
                mime: format!("image/{}", "a".repeat(122)),
                bytes: vec![1; 24],
            },
            PackagedExportAsset {
                filename: "b".repeat(180),
                mime: format!("image/{}", "b".repeat(122)),
                bytes: vec![2; 24],
            },
        ],
    };

    assert!(GotenbergRenderer::validate_package(&package, max_package).is_ok());
    assert_eq!(
        GotenbergRenderer::validate_package(&package, max_package - 1),
        Err(PdfRendererError::PackageLimitExceeded)
    );
}

#[test]
fn renderer_rejects_oversized_multipart_metadata_before_submission() {
    let mut oversized_filename = package();
    oversized_filename.assets[0].filename = format!("{}.png", "a".repeat(512));
    assert_eq!(
        GotenbergRenderer::validate_package(&oversized_filename, usize::MAX),
        Err(PdfRendererError::PackageLimitExceeded)
    );

    let mut oversized_mime = package();
    oversized_mime.assets[0].mime = format!("image/png; x={}", "a".repeat(512));
    assert_eq!(
        GotenbergRenderer::validate_package(&oversized_mime, usize::MAX),
        Err(PdfRendererError::PackageLimitExceeded)
    );
}

#[tokio::test]
async fn renderer_posts_once_to_the_fixed_html_conversion_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/forms/chromium/convert/html"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/pdf; charset=binary")
                .set_body_bytes(b"%PDF-1.7\nfixture"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let renderer = GotenbergRenderer::new(&server.uri(), 1024).unwrap();

    let pdf = renderer
        .render(&package(), Duration::from_secs(2))
        .await
        .unwrap();

    assert_eq!(pdf, b"%PDF-1.7\nfixture");
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("name=\"files\""));
    assert!(body.contains("filename=\"index.html\""));
    assert!(body.contains("filename=\"footer.html\""));
    assert!(body.contains("filename=\"asset-0001.png\""));
}

#[tokio::test]
async fn renderer_rejects_non_pdf_and_suppresses_diagnostic_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/forms/chromium/convert/html"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("content-type", "text/html")
                .set_body_string("SECRET RENDERER DIAGNOSTIC"),
        )
        .mount(&server)
        .await;
    let renderer = GotenbergRenderer::new(&server.uri(), 1024).unwrap();

    let error = renderer
        .render(&package(), Duration::from_secs(2))
        .await
        .unwrap_err();

    assert_eq!(error, PdfRendererError::ConversionFailed);
    assert!(!format!("{error:?}").contains("SECRET"));
    assert!(!error.to_string().contains("SECRET"));
}

#[tokio::test]
async fn renderer_enforces_declared_and_streamed_response_limits() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/forms/chromium/convert/html"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/pdf")
                .insert_header("content-length", "33")
                .set_body_bytes([b"%PDF-".as_slice(), &[0_u8; 28]].concat()),
        )
        .mount(&server)
        .await;
    let renderer = GotenbergRenderer::new(&server.uri(), 32).unwrap();
    assert_eq!(
        renderer
            .render(&package(), Duration::from_secs(2))
            .await
            .unwrap_err(),
        PdfRendererError::ResponseLimitExceeded
    );
}

async fn chunked_server(chunks: Vec<Vec<u8>>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(move || {
        let chunks = chunks.clone();
        async move {
            let stream = futures::stream::iter(
                chunks
                    .into_iter()
                    .map(|chunk| Ok::<_, Infallible>(axum::body::Bytes::from(chunk))),
            );
            (
                [("content-type", "application/pdf")],
                axum::body::Body::from_stream(stream),
            )
        }
    });
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn renderer_accepts_exact_declared_and_chunked_limits() {
    let exact = [b"%PDF-".as_slice(), &[0_u8; 27]].concat();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/pdf")
                .insert_header("content-length", "32")
                .set_body_bytes(exact.clone()),
        )
        .mount(&server)
        .await;
    assert_eq!(
        GotenbergRenderer::new(&server.uri(), 32)
            .unwrap()
            .render(&package(), Duration::from_secs(2))
            .await
            .unwrap(),
        exact
    );

    let (url, task) = chunked_server(vec![b"%PDF-".to_vec(), vec![0; 27]]).await;
    let pdf = GotenbergRenderer::new(&url, 32)
        .unwrap()
        .render(&package(), Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(pdf.len(), 32);
    task.abort();
}

#[tokio::test]
async fn renderer_rejects_chunked_limit_plus_one_and_huge_single_chunk() {
    for chunks in [
        vec![b"%PDF-".to_vec(), vec![0; 28]],
        vec![[b"%PDF-".as_slice(), &vec![0; 1024 * 1024]].concat()],
    ] {
        let (url, task) = chunked_server(chunks).await;
        let result = GotenbergRenderer::new(&url, 32)
            .unwrap()
            .render(&package(), Duration::from_secs(2))
            .await;
        assert_eq!(result.unwrap_err(), PdfRendererError::ResponseLimitExceeded);
        task.abort();
    }
}

#[tokio::test]
async fn renderer_validates_success_mime_signature_and_transport_without_body_leaks() {
    for (content_type, body) in [
        ("text/plain", b"%PDF-secret".as_slice()),
        ("application/pdf", b"SECRET not a PDF".as_slice()),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", content_type)
                    .set_body_bytes(body),
            )
            .mount(&server)
            .await;
        let error = GotenbergRenderer::new(&server.uri(), 1024)
            .unwrap()
            .render(&package(), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(error, PdfRendererError::InvalidResponse);
        assert!(!format!("{error:?}").contains("SECRET"));
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        GotenbergRenderer::new(&url, 1024)
            .unwrap()
            .render(&package(), Duration::from_secs(2))
            .await
            .unwrap_err(),
        PdfRendererError::Unavailable
    );
}

#[tokio::test]
async fn temporary_upstream_statuses_are_unavailable_and_other_failures_are_conversion_errors() {
    for (status, expected) in [
        (429, PdfRendererError::Unavailable),
        (503, PdfRendererError::Unavailable),
        (504, PdfRendererError::Unavailable),
        (400, PdfRendererError::ConversionFailed),
        (500, PdfRendererError::ConversionFailed),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_string("SECRET upstream body"))
            .mount(&server)
            .await;
        let error = GotenbergRenderer::new(&server.uri(), 1024)
            .unwrap()
            .render(&package(), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("SECRET"));
    }
}

#[test]
fn renderer_rejects_unsafe_base_urls_and_package_names() {
    assert!(GotenbergRenderer::new("file:///tmp/renderer", 1024).is_err());
    assert!(GotenbergRenderer::new("http://user:secret@localhost:3000", 1024).is_err());

    let mut invalid = package();
    invalid.assets[0].filename = "../secret.png".into();
    assert!(GotenbergRenderer::validate_package(&invalid, 1024).is_err());

    let mut duplicate = package();
    duplicate.assets.push(duplicate.assets[0].clone());
    assert_eq!(
        GotenbergRenderer::validate_package(&duplicate, 1024).unwrap_err(),
        PdfRendererError::InvalidPackage
    );
}

#[tokio::test]
async fn renderer_does_not_follow_redirects_or_retry_failures() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/forms/chromium/convert/html"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "/unexpected-second-request"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let renderer = GotenbergRenderer::new(&server.uri(), 1024).unwrap();

    assert_eq!(
        renderer
            .render(&package(), Duration::from_secs(2))
            .await
            .unwrap_err(),
        PdfRendererError::ConversionFailed
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn renderer_timeout_cancels_the_request_future() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/forms/chromium/convert/html"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/pdf")
                .set_body_bytes(b"%PDF-late")
                .set_delay(Duration::from_millis(200)),
        )
        .mount(&server)
        .await;
    let renderer = GotenbergRenderer::new(&server.uri(), 1024).unwrap();

    assert_eq!(
        renderer
            .render(&package(), Duration::from_millis(25))
            .await
            .unwrap_err(),
        PdfRendererError::Timeout
    );
}
