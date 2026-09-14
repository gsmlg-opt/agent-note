use super::document::ExportDocumentPackage;
use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{multipart, redirect::Policy, Client, Url};
use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

const CONVERT_PATH: &str = "forms/chromium/convert/html";
const DEFAULT_MAX_PACKAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_INDEX_HTML_BYTES: usize = 8 * 1024 * 1024;
const MAX_FOOTER_HTML_BYTES: usize = 1024;
const MAX_MULTIPART_FILENAME_BYTES: usize = 180;
const MAX_MULTIPART_MIME_BYTES: usize = 128;
const MAX_MULTIPART_BOUNDARY_BYTES: usize = 128;
const MULTIPART_FIELD_NAME: &str = "files";
const CONVERSION_FIELDS: [(&str, &str); 4] = [
    ("preferCssPageSize", "true"),
    ("printBackground", "true"),
    ("skipNetworkIdleEvent", "false"),
    ("failOnResourceLoadingFailed", "true"),
];

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

    /// Returns the renderer request cap needed for independently valid document and asset
    /// limits. The reserve covers multipart boundaries and validated per-part metadata.
    pub fn max_package_bytes_for_export(
        max_combined_asset_bytes: usize,
        max_asset_count: usize,
    ) -> Result<usize, PdfRendererError> {
        max_renderer_package_bytes(
            MAX_INDEX_HTML_BYTES,
            MAX_FOOTER_HTML_BYTES,
            max_combined_asset_bytes,
            max_asset_count,
        )
    }

    pub fn validate_package(
        package: &ExportDocumentPackage,
        max_bytes: usize,
    ) -> Result<usize, PdfRendererError> {
        if max_bytes == 0 {
            return Err(PdfRendererError::InvalidPackage);
        }
        let mut payload_bytes = package
            .index_html
            .len()
            .checked_add(package.footer_html.len())
            .ok_or(PdfRendererError::PackageLimitExceeded)?;
        if package.assets.len() > 64 {
            return Err(PdfRendererError::PackageLimitExceeded);
        }
        let mut filenames = HashSet::with_capacity(package.assets.len());
        for asset in &package.assets {
            if asset.filename.len() > MAX_MULTIPART_FILENAME_BYTES
                || asset.mime.len() > MAX_MULTIPART_MIME_BYTES
            {
                return Err(PdfRendererError::PackageLimitExceeded);
            }
            if !is_flat_asset_name(&asset.filename)
                || !filenames.insert(asset.filename.as_str())
                || !asset.mime.starts_with("image/")
                || !is_safe_multipart_metadata(&asset.filename, MAX_MULTIPART_FILENAME_BYTES)
                || !is_safe_multipart_metadata(&asset.mime, MAX_MULTIPART_MIME_BYTES)
            {
                return Err(PdfRendererError::InvalidPackage);
            }
            payload_bytes = payload_bytes
                .checked_add(asset.bytes.len())
                .ok_or(PdfRendererError::PackageLimitExceeded)?;
        }
        let multipart_bytes = multipart_body_bytes(package, MAX_MULTIPART_BOUNDARY_BYTES)?;
        if payload_bytes > max_bytes || multipart_bytes > max_bytes {
            return Err(PdfRendererError::PackageLimitExceeded);
        }
        Ok(multipart_bytes)
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
        for (field, value) in CONVERSION_FIELDS {
            form = form.text(field, value);
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
            return Err(if matches!(response.status().as_u16(), 429 | 503 | 504) {
                PdfRendererError::Unavailable
            } else {
                PdfRendererError::ConversionFailed
            });
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
        let initial_capacity = self.max_pdf_bytes.min(64 * 1024).saturating_add(1);
        let mut output = Vec::with_capacity(initial_capacity);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(classify_transport)?;
            append_response_chunk(&mut output, &chunk, self.max_pdf_bytes)?;
        }
        if !output.starts_with(b"%PDF-") {
            return Err(PdfRendererError::InvalidResponse);
        }
        Ok(output)
    }
}

fn append_response_chunk(
    output: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> Result<(), PdfRendererError> {
    let ceiling = max_bytes
        .checked_add(1)
        .ok_or(PdfRendererError::ResponseLimitExceeded)?;
    let remaining = ceiling
        .checked_sub(output.len())
        .ok_or(PdfRendererError::ResponseLimitExceeded)?;
    let take = remaining.min(chunk.len());
    output.extend_from_slice(&chunk[..take]);
    if output.len() > max_bytes || take < chunk.len() {
        return Err(PdfRendererError::ResponseLimitExceeded);
    }
    Ok(())
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

fn is_safe_multipart_metadata(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte) && !matches!(byte, b'"' | b'\\'))
}

/// Calculates the exact size emitted by reqwest's default multipart layout for a supplied
/// boundary length. Callers use the maximum supported boundary length as a conservative bound.
fn multipart_body_bytes(
    package: &ExportDocumentPackage,
    boundary_len: usize,
) -> Result<usize, PdfRendererError> {
    if boundary_len > MAX_MULTIPART_BOUNDARY_BYTES {
        return Err(PdfRendererError::PackageLimitExceeded);
    }
    let mut bytes = multipart_part_bytes(
        boundary_len,
        "index.html",
        "text/html; charset=utf-8",
        package.index_html.len(),
    )?
    .checked_add(multipart_part_bytes(
        boundary_len,
        "footer.html",
        "text/html; charset=utf-8",
        package.footer_html.len(),
    )?)
    .ok_or(PdfRendererError::PackageLimitExceeded)?;
    for asset in &package.assets {
        bytes = bytes
            .checked_add(multipart_part_bytes(
                boundary_len,
                &asset.filename,
                &asset.mime,
                asset.bytes.len(),
            )?)
            .ok_or(PdfRendererError::PackageLimitExceeded)?;
    }
    for (field, value) in CONVERSION_FIELDS {
        bytes = bytes
            .checked_add(multipart_text_part_bytes(boundary_len, field, value.len())?)
            .ok_or(PdfRendererError::PackageLimitExceeded)?;
    }
    bytes
        .checked_add(closing_boundary_bytes(boundary_len)?)
        .ok_or(PdfRendererError::PackageLimitExceeded)
}

fn opening_boundary_bytes(boundary_len: usize) -> Result<usize, PdfRendererError> {
    2_usize
        .checked_add(boundary_len)
        .and_then(|size| size.checked_add(2)) // --boundary\r\n
        .ok_or(PdfRendererError::PackageLimitExceeded)
}

fn closing_boundary_bytes(boundary_len: usize) -> Result<usize, PdfRendererError> {
    2_usize
        .checked_add(boundary_len)
        .and_then(|size| size.checked_add(4)) // --boundary--\r\n
        .ok_or(PdfRendererError::PackageLimitExceeded)
}

fn multipart_text_part_bytes(
    boundary_len: usize,
    field_name: &str,
    payload_len: usize,
) -> Result<usize, PdfRendererError> {
    let header_len = b"Content-Disposition: form-data; name=\""
        .len()
        .checked_add(field_name.len())
        .and_then(|size| size.checked_add(b"\"\r\n".len()))
        .ok_or(PdfRendererError::PackageLimitExceeded)?;
    opening_boundary_bytes(boundary_len)?
        .checked_add(header_len)
        .and_then(|size| size.checked_add(2)) // blank \r\n
        .and_then(|size| size.checked_add(payload_len))
        .and_then(|size| size.checked_add(2)) // trailing \r\n
        .ok_or(PdfRendererError::PackageLimitExceeded)
}

fn multipart_part_bytes(
    boundary_len: usize,
    filename: &str,
    mime: &str,
    payload_len: usize,
) -> Result<usize, PdfRendererError> {
    // reqwest's default PathSegment encoder leaves the constant field name `files` unchanged.
    let header_len = b"Content-Disposition: form-data; name=\""
        .len()
        .checked_add(MULTIPART_FIELD_NAME.len())
        .and_then(|size| size.checked_add(b"\"; filename=\"".len()))
        .and_then(|size| size.checked_add(filename.len()))
        .and_then(|size| size.checked_add(b"\"\r\nContent-Type: ".len()))
        .and_then(|size| size.checked_add(mime.len()))
        .ok_or(PdfRendererError::PackageLimitExceeded)?;
    opening_boundary_bytes(boundary_len)?
        .checked_add(header_len)
        .and_then(|size| size.checked_add(4)) // \r\n\r\n
        .and_then(|size| size.checked_add(payload_len))
        .and_then(|size| size.checked_add(2)) // trailing \r\n
        .ok_or(PdfRendererError::PackageLimitExceeded)
}

pub fn max_renderer_package_bytes(
    max_index_html_bytes: usize,
    max_footer_html_bytes: usize,
    max_combined_asset_bytes: usize,
    max_asset_count: usize,
) -> Result<usize, PdfRendererError> {
    if max_asset_count > 64 {
        return Err(PdfRendererError::InvalidConfiguration);
    }
    let fixed = multipart_part_bytes(
        MAX_MULTIPART_BOUNDARY_BYTES,
        "index.html",
        "text/html; charset=utf-8",
        max_index_html_bytes,
    )?
    .checked_add(multipart_part_bytes(
        MAX_MULTIPART_BOUNDARY_BYTES,
        "footer.html",
        "text/html; charset=utf-8",
        max_footer_html_bytes,
    )?)
    .ok_or(PdfRendererError::InvalidConfiguration)?;
    let per_asset_overhead = multipart_part_bytes(
        MAX_MULTIPART_BOUNDARY_BYTES,
        &"a".repeat(MAX_MULTIPART_FILENAME_BYTES),
        &"i".repeat(MAX_MULTIPART_MIME_BYTES),
        0,
    )?;
    let fixed = CONVERSION_FIELDS.iter().try_fold(
        fixed,
        |size, (field, value)| -> Result<usize, PdfRendererError> {
            size.checked_add(multipart_text_part_bytes(
                MAX_MULTIPART_BOUNDARY_BYTES,
                field,
                value.len(),
            )?)
            .ok_or(PdfRendererError::InvalidConfiguration)
        },
    )?;
    let closing = closing_boundary_bytes(MAX_MULTIPART_BOUNDARY_BYTES)
        .map_err(|_| PdfRendererError::InvalidConfiguration)?;
    fixed
        .checked_add(
            per_asset_overhead
                .checked_mul(max_asset_count)
                .ok_or(PdfRendererError::InvalidConfiguration)?,
        )
        .and_then(|size| size.checked_add(max_combined_asset_bytes))
        .and_then(|size| size.checked_add(closing))
        .ok_or(PdfRendererError::InvalidConfiguration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::document::PackagedExportAsset;

    #[test]
    fn bounded_append_never_grows_output_beyond_limit_plus_one() {
        let mut output = b"%PDF-".to_vec();
        let error = append_response_chunk(&mut output, &vec![0; 1024 * 1024], 32).unwrap_err();
        assert_eq!(error, PdfRendererError::ResponseLimitExceeded);
        assert_eq!(output.len(), 33);
        assert!(output.capacity() <= 33);
    }

    #[test]
    fn multipart_preflight_counts_the_exact_boundary_and_headers() {
        let package = ExportDocumentPackage {
            index_html: "i".into(),
            footer_html: "f".into(),
            assets: vec![PackagedExportAsset {
                filename: "a.png".into(),
                mime: "image/png".into(),
                bytes: vec![7],
            }],
        };

        let expected = concat!(
            "--1234567\r\n",
            "Content-Disposition: form-data; name=\"files\"; filename=\"index.html\"\r\n",
            "Content-Type: text/html; charset=utf-8\r\n\r\n",
            "i\r\n",
            "--1234567\r\n",
            "Content-Disposition: form-data; name=\"files\"; filename=\"footer.html\"\r\n",
            "Content-Type: text/html; charset=utf-8\r\n\r\n",
            "f\r\n",
            "--1234567\r\n",
            "Content-Disposition: form-data; name=\"files\"; filename=\"a.png\"\r\n",
            "Content-Type: image/png\r\n\r\n",
            "\x07\r\n",
            "--1234567\r\n",
            "Content-Disposition: form-data; name=\"preferCssPageSize\"\r\n\r\n",
            "true\r\n",
            "--1234567\r\n",
            "Content-Disposition: form-data; name=\"printBackground\"\r\n\r\n",
            "true\r\n",
            "--1234567\r\n",
            "Content-Disposition: form-data; name=\"skipNetworkIdleEvent\"\r\n\r\n",
            "false\r\n",
            "--1234567\r\n",
            "Content-Disposition: form-data; name=\"failOnResourceLoadingFailed\"\r\n\r\n",
            "true\r\n",
            "--1234567--\r\n"
        );

        assert_eq!(multipart_body_bytes(&package, 7).unwrap(), expected.len());
    }

    #[test]
    fn multipart_framing_accounting_rejects_arithmetic_overflow() {
        assert_eq!(
            multipart_part_bytes(usize::MAX, "a.png", "image/png", 1),
            Err(PdfRendererError::PackageLimitExceeded)
        );
        assert_eq!(
            multipart_text_part_bytes(usize::MAX, "printBackground", 4),
            Err(PdfRendererError::PackageLimitExceeded)
        );
    }
}
