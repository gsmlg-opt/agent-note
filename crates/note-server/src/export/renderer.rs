use super::document::ExportDocumentPackage;
use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{multipart, redirect::Policy, Client, Url};
use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

const CONVERT_PATH: &str = "forms/chromium/convert/html";
const DEFAULT_MAX_PACKAGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfRendererError {
    InvalidConfiguration,
    InvalidPackage,
    PackageLimitExceeded,
    Unavailable,
    ConversionFailed,
    InvalidResponse,
    ResponseLimitExceeded,
    Timeout,
}

impl fmt::Display for PdfRendererError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "PDF renderer configuration is invalid",
            Self::InvalidPackage => "PDF renderer package is invalid",
            Self::PackageLimitExceeded => "PDF renderer package exceeds the byte limit",
            Self::Unavailable => "PDF renderer is unavailable",
            Self::ConversionFailed => "PDF renderer conversion failed",
            Self::InvalidResponse => "PDF renderer returned an invalid response",
            Self::ResponseLimitExceeded => "PDF renderer response exceeds the byte limit",
            Self::Timeout => "PDF renderer request timed out",
        })
    }
}

impl std::error::Error for PdfRendererError {}

#[async_trait]
pub trait PdfRenderer: Send + Sync {
    async fn render(
        &self,
        package: &ExportDocumentPackage,
        timeout: Duration,
    ) -> Result<Vec<u8>, PdfRendererError>;
}

pub struct GotenbergRenderer {
    client: Client,
    endpoint: Url,
    max_pdf_bytes: usize,
    max_package_bytes: usize,
}

impl GotenbergRenderer {
    pub fn new(base_url: &str, max_pdf_bytes: usize) -> Result<Self, PdfRendererError> {
        Self::with_limits(base_url, max_pdf_bytes, DEFAULT_MAX_PACKAGE_BYTES)
    }

    pub fn with_limits(
        base_url: &str,
        max_pdf_bytes: usize,
        max_package_bytes: usize,
    ) -> Result<Self, PdfRendererError> {
        if max_pdf_bytes == 0 || max_package_bytes == 0 {
            return Err(PdfRendererError::InvalidConfiguration);
        }
        let mut base = Url::parse(base_url).map_err(|_| PdfRendererError::InvalidConfiguration)?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(PdfRendererError::InvalidConfiguration);
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let endpoint = base
            .join(CONVERT_PATH)
            .map_err(|_| PdfRendererError::InvalidConfiguration)?;
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| PdfRendererError::InvalidConfiguration)?;
        Ok(Self {
            client,
            endpoint,
            max_pdf_bytes,
            max_package_bytes,
        })
    }

    pub fn validate_package(
        package: &ExportDocumentPackage,
        max_bytes: usize,
    ) -> Result<usize, PdfRendererError> {
        if max_bytes == 0 {
            return Err(PdfRendererError::InvalidPackage);
        }
        let mut total = package
            .index_html
            .len()
            .checked_add(package.footer_html.len())
            .ok_or(PdfRendererError::PackageLimitExceeded)?;
        if package.assets.len() > 64 {
            return Err(PdfRendererError::PackageLimitExceeded);
        }
        let mut filenames = HashSet::with_capacity(package.assets.len());
        for asset in &package.assets {
            if !is_flat_asset_name(&asset.filename)
                || !filenames.insert(asset.filename.as_str())
                || !asset.mime.starts_with("image/")
            {
                return Err(PdfRendererError::InvalidPackage);
            }
            total = total
                .checked_add(asset.bytes.len())
                .ok_or(PdfRendererError::PackageLimitExceeded)?;
        }
        if total > max_bytes {
            return Err(PdfRendererError::PackageLimitExceeded);
        }
        Ok(total)
    }

    async fn render_inner(
        &self,
        package: &ExportDocumentPackage,
        timeout: Duration,
    ) -> Result<Vec<u8>, PdfRendererError> {
        Self::validate_package(package, self.max_package_bytes)?;
        let mut form = multipart::Form::new()
            .part(
                "files",
                multipart::Part::text(package.index_html.clone())
                    .file_name("index.html")
                    .mime_str("text/html; charset=utf-8")
                    .map_err(|_| PdfRendererError::InvalidPackage)?,
            )
            .part(
                "files",
                multipart::Part::text(package.footer_html.clone())
                    .file_name("footer.html")
                    .mime_str("text/html; charset=utf-8")
                    .map_err(|_| PdfRendererError::InvalidPackage)?,
            );
        for asset in &package.assets {
            form = form.part(
                "files",
                multipart::Part::bytes(asset.bytes.clone())
                    .file_name(asset.filename.clone())
                    .mime_str(&asset.mime)
                    .map_err(|_| PdfRendererError::InvalidPackage)?,
            );
        }
        let response = self
            .client
            .post(self.endpoint.clone())
            .timeout(timeout)
            .multipart(form)
            .send()
            .await
            .map_err(classify_transport)?;
        if !response.status().is_success() {
            return Err(PdfRendererError::ConversionFailed);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        if !content_type.is_some_and(|value| value.eq_ignore_ascii_case("application/pdf")) {
            return Err(PdfRendererError::InvalidResponse);
        }
        if response
            .content_length()
            .is_some_and(|size| size > self.max_pdf_bytes as u64)
        {
            return Err(PdfRendererError::ResponseLimitExceeded);
        }
        let mut output = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(classify_transport)?;
            let next = output
                .len()
                .checked_add(chunk.len())
                .ok_or(PdfRendererError::ResponseLimitExceeded)?;
            if next > self.max_pdf_bytes {
                return Err(PdfRendererError::ResponseLimitExceeded);
            }
            output.extend_from_slice(&chunk);
        }
        if !output.starts_with(b"%PDF-") {
            return Err(PdfRendererError::InvalidResponse);
        }
        Ok(output)
    }
}

#[async_trait]
impl PdfRenderer for GotenbergRenderer {
    async fn render(
        &self,
        package: &ExportDocumentPackage,
        timeout: Duration,
    ) -> Result<Vec<u8>, PdfRendererError> {
        tokio::time::timeout(timeout, self.render_inner(package, timeout))
            .await
            .map_err(|_| PdfRendererError::Timeout)?
    }
}

fn classify_transport(error: reqwest::Error) -> PdfRendererError {
    if error.is_timeout() {
        PdfRendererError::Timeout
    } else {
        PdfRendererError::Unavailable
    }
}

fn is_flat_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name != "index.html"
        && name != "footer.html"
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
        && !name.chars().any(char::is_control)
}
