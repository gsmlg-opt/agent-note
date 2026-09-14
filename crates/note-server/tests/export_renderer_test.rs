use note_server::export::document::{ExportDocumentPackage, PackagedExportAsset};
use note_server::export::renderer::{GotenbergRenderer, PdfRenderer, PdfRendererError};
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
    for response in [
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/pdf")
            .set_body_bytes([b"%PDF-".as_slice(), &[0_u8; 64]].concat()),
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/pdf")
            .set_body_bytes([b"%PDF-".as_slice(), &[0_u8; 64]].concat()),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/forms/chromium/convert/html"))
            .respond_with(response)
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
