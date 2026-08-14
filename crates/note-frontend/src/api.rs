use crate::state::{
    AttachmentContent, DeletedNoteSummary, LabelFilter, LabelKey, NoteAttachment, NoteSummary,
    SearchResultSummary, SystemConfig, SystemInfo,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use gloo_net::http::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct NoteMutationApiError {
    pub code: String,
    pub message: String,
    pub details: Value,
    pub retryable: bool,
    #[serde(skip)]
    pub status: Option<u16>,
}

impl NoteMutationApiError {
    fn transport() -> Self {
        Self {
            code: "transport_error".into(),
            message: "The note service could not be reached".into(),
            details: Value::Object(Default::default()),
            retryable: true,
            status: None,
        }
    }

    fn unexpected(status: u16) -> Self {
        Self {
            code: "unexpected_response".into(),
            message: "The note service returned an unexpected response".into(),
            details: Value::Object(Default::default()),
            retryable: status >= 500,
            status: Some(status),
        }
    }

    pub fn is_stale_revision(&self) -> bool {
        self.code == "stale_revision"
    }
}

impl fmt::Display for NoteMutationApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

fn decode_mutation_error(status: u16, body: &str) -> NoteMutationApiError {
    let mut error = serde_json::from_str::<NoteMutationApiError>(body)
        .unwrap_or_else(|_| NoteMutationApiError::unexpected(status));
    error.status = Some(status);
    error
}

async fn ok_or_mutation_error(resp: Response) -> Result<Response, NoteMutationApiError> {
    if resp.ok() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(decode_mutation_error(status, &body))
}

// gloo-net (like the Fetch API) does NOT return Err on a 4xx/5xx status — .send() resolves fine and
// only the body read would fail. So without this check, a server error (e.g. "empty title") gets
// deserialized as the success DTO, failing with a confusing serde error instead of the real message.
// This reads the plain-text error body note-server returns and surfaces it. Errors are Strings so the
// server's actual message reaches the UI.
async fn ok_or_body_error(resp: Response) -> Result<Response, String> {
    if resp.ok() {
        Ok(resp)
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("server error {status}: {body}"))
    }
}

// ---- System ----

pub async fn get_system_config() -> Result<SystemConfig, String> {
    let resp = Request::get("/api/system/config")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn update_system_config(config: &SystemConfig) -> Result<(), String> {
    let resp = Request::put("/api/system/config")
        .json(config)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
}

pub async fn get_system_info() -> Result<SystemInfo, String> {
    let resp = Request::get("/api/system/info")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

// ---- Notes ----

pub fn attachment_url(base: &str, path: &str) -> String {
    let encoded_path = path
        .trim_start_matches("./")
        .trim_start_matches('/')
        .split('/')
        .map(urlencoding::encode)
        .collect::<Vec<_>>()
        .join("/");
    format!("{}/{encoded_path}", base.trim_end_matches('/'))
}

#[derive(Deserialize)]
struct SearchResultDto {
    id: String,
    title: String,
    score: f32,
}

pub fn label_filter_selector(filters: &[LabelFilter]) -> Option<String> {
    let terms = filters
        .iter()
        .filter(|filter| !filter.key.trim().is_empty())
        .map(|filter| match filter.value.trim() {
            "" => filter.key.trim().to_string(),
            value => {
                format!("{}{}{}", filter.key.trim(), filter.operator.trim(), value)
            }
        })
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join("&"))
    }
}

pub async fn search_filtered(
    query: &str,
    limit: usize,
    filters: &[LabelFilter],
) -> Result<Vec<SearchResultSummary>, String> {
    let mut body = serde_json::json!({ "query": query, "limit": limit });
    if let Some(selector) = label_filter_selector(filters) {
        body["label"] = serde_json::Value::String(selector);
    }
    // POST, not GET: the browser Fetch API forbids a body on GET requests, and search params go in
    // the JSON body. note-server's /api/notes/search is registered as POST for this reason.
    let resp = Request::post("/api/notes/search")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let dtos: Vec<SearchResultDto> = ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(dtos
        .into_iter()
        .map(|d| SearchResultSummary {
            id: d.id,
            title: d.title,
            // Guard against a non-finite score from a malformed payload (breaks PartialEq/render).
            score: if d.score.is_finite() { d.score } else { 0.0 },
        })
        .collect())
}

pub async fn save_note(
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    labels: &[(String, String)],
) -> Result<String, String> {
    let attachments = attachments
        .iter()
        .map(NoteAttachmentRequestDto::from)
        .collect::<Vec<_>>();
    let body = serde_json::json!({
        "title": title,
        "content": content,
        "attachments": attachments,
        "labels": labels
    });
    #[derive(Deserialize)]
    struct SaveResp {
        id: String,
    }
    let resp = Request::post("/api/notes")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let resp: SaveResp = ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(resp.id)
}

#[derive(Serialize)]
struct NoteAttachmentRequestDto<'a> {
    id: &'a str,
    path: &'a str,
    mime: &'a str,
    description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_base64: Option<&'a str>,
}

impl<'a> From<&'a NoteAttachment> for NoteAttachmentRequestDto<'a> {
    fn from(attachment: &'a NoteAttachment) -> Self {
        let (content, content_base64) = match &attachment.content {
            AttachmentContent::Text(content) => (Some(content.as_str()), None),
            AttachmentContent::Base64(content) => (None, Some(content.as_str())),
        };
        Self {
            id: &attachment.id,
            path: &attachment.path,
            mime: &attachment.mime,
            description: &attachment.description,
            content,
            content_base64,
        }
    }
}

#[derive(Deserialize)]
struct NoteAttachmentMetadataDto {
    id: String,
    path: String,
    mime: String,
    #[serde(default)]
    description: String,
}

fn attachment_content_from_bytes(bytes: Vec<u8>) -> AttachmentContent {
    match String::from_utf8(bytes) {
        Ok(content) => AttachmentContent::Text(content),
        Err(error) => AttachmentContent::Base64(STANDARD.encode(error.into_bytes())),
    }
}

async fn fetch_note_attachment(
    note_id: &str,
    metadata: NoteAttachmentMetadataDto,
) -> Result<NoteAttachment, String> {
    let base = format!("/api/notes/{note_id}/attachments");
    let url = attachment_url(&base, &metadata.path);
    let resp = Request::get(&url).send().await.map_err(|e| e.to_string())?;
    let bytes = ok_or_body_error(resp)
        .await?
        .binary()
        .await
        .map_err(|e| e.to_string())?;
    Ok(NoteAttachment {
        id: metadata.id,
        path: metadata.path,
        mime: metadata.mime,
        description: metadata.description,
        content: attachment_content_from_bytes(bytes),
    })
}

#[derive(Deserialize)]
struct NoteDto {
    id: String,
    title: String,
    content: String,
    #[serde(default)]
    attachments: Vec<NoteAttachmentMetadataDto>,
    labels: Vec<(String, String)>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    updated_at: i64,
    revision: i64,
}

#[derive(Deserialize)]
struct NoteListDto {
    id: String,
    title: String,
    labels: Vec<(String, String)>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    updated_at: i64,
    revision: i64,
}

pub struct NotesPage {
    pub notes: Vec<NoteSummary>,
    pub total: usize,
}

#[derive(Deserialize)]
struct CountNotesDto {
    total: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DashboardLabel {
    pub key: String,
    pub description: String,
    pub value_type: String,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DashboardNote {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DashboardEmbeddingNote {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DashboardSummary {
    pub note_count: usize,
    pub embedded_note_count: usize,
    pub embedding_note: Option<DashboardEmbeddingNote>,
    pub label_count: usize,
    pub last_updated_at: Option<i64>,
    pub labels: Vec<DashboardLabel>,
    pub recent_updates: Vec<DashboardNote>,
}

fn notes_list_url(filters: &[LabelFilter], limit: Option<usize>, offset: Option<usize>) -> String {
    let mut params = Vec::new();
    if let Some(selector) = label_filter_selector(filters) {
        params.push(format!("label={}", urlencoding::encode(&selector)));
    }
    if let Some(limit) = limit {
        params.push(format!("limit={limit}"));
    }
    if let Some(offset) = offset {
        params.push(format!("offset={offset}"));
    }
    if params.is_empty() {
        "/api/notes".to_string()
    } else {
        format!("/api/notes?{}", params.join("&"))
    }
}

fn notes_count_url(filters: &[LabelFilter]) -> String {
    match label_filter_selector(filters) {
        Some(selector) => format!("/api/notes/count?label={}", urlencoding::encode(&selector)),
        None => "/api/notes/count".to_string(),
    }
}

async fn fetch_note_summaries(
    filters: &[LabelFilter],
    limit: usize,
    offset: usize,
) -> Result<Vec<NoteSummary>, String> {
    let url = notes_list_url(filters, Some(limit), Some(offset));
    let resp = Request::get(&url).send().await.map_err(|e| e.to_string())?;
    let dtos: Vec<NoteListDto> = ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(dtos
        .into_iter()
        .map(|d| NoteSummary {
            id: d.id,
            title: d.title,
            content: String::new(),
            attachments: vec![],
            labels: d.labels,
            created_at: d.created_at,
            updated_at: d.updated_at,
            revision: d.revision,
        })
        .collect())
}

pub async fn count_notes_filtered(filters: &[LabelFilter]) -> Result<usize, String> {
    let resp = Request::get(&notes_count_url(filters))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let count: CountNotesDto = ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(count.total)
}

pub async fn dashboard() -> Result<DashboardSummary, String> {
    let resp = Request::get("/api/dashboard")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn list_notes_page(
    filters: &[LabelFilter],
    limit: usize,
    offset: usize,
) -> Result<NotesPage, String> {
    let notes = fetch_note_summaries(filters, limit, offset).await?;
    let total = count_notes_filtered(filters).await?;

    Ok(NotesPage { notes, total })
}

pub async fn get_note(id: &str) -> Result<NoteSummary, String> {
    let resp = Request::get(&format!("/api/notes/{id}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let d: NoteDto = ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let mut attachments = Vec::with_capacity(d.attachments.len());
    for attachment in d.attachments {
        attachments.push(fetch_note_attachment(&d.id, attachment).await?);
    }
    Ok(NoteSummary {
        id: d.id,
        title: d.title,
        content: d.content,
        attachments,
        labels: d.labels,
        created_at: d.created_at,
        updated_at: d.updated_at,
        revision: d.revision,
    })
}

fn update_note_body(
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    labels: &[(String, String)],
    expected_revision: i64,
) -> Value {
    let attachments = attachments
        .iter()
        .map(NoteAttachmentRequestDto::from)
        .collect::<Vec<_>>();
    serde_json::json!({
        "expected_revision": expected_revision,
        "title": title,
        "content": content,
        "attachments": attachments,
        "labels": labels
    })
}

fn mutation_url(base: &str, expected_revision: i64) -> String {
    format!("{base}?expected_revision={expected_revision}")
}

pub async fn update_note(
    id: &str,
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    labels: &[(String, String)],
    expected_revision: i64,
) -> Result<i64, NoteMutationApiError> {
    let body = update_note_body(title, content, attachments, labels, expected_revision);
    let resp = Request::put(&format!("/api/notes/{id}"))
        .json(&body)
        .map_err(|_| NoteMutationApiError::transport())?
        .send()
        .await
        .map_err(|_| NoteMutationApiError::transport())?;
    let note: NoteDto = ok_or_mutation_error(resp)
        .await?
        .json()
        .await
        .map_err(|_| NoteMutationApiError::unexpected(200))?;
    Ok(note.revision)
}

pub async fn delete_note(id: &str, expected_revision: i64) -> Result<(), NoteMutationApiError> {
    let resp = Request::delete(&mutation_url(
        &format!("/api/notes/{id}"),
        expected_revision,
    ))
    .send()
    .await
    .map_err(|_| NoteMutationApiError::transport())?;
    ok_or_mutation_error(resp).await.map(|_| ())
}

#[derive(Deserialize)]
struct TrashNoteDto {
    id: String,
    title: String,
    labels: Vec<(String, String)>,
    created_at: i64,
    updated_at: i64,
    deleted_at: i64,
    revision: i64,
}

pub async fn list_deleted_notes() -> Result<Vec<DeletedNoteSummary>, String> {
    let resp = Request::get("/api/trash")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let notes: Vec<TrashNoteDto> = ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(notes
        .into_iter()
        .map(|note| DeletedNoteSummary {
            id: note.id,
            title: note.title,
            labels: note.labels,
            created_at: note.created_at,
            updated_at: note.updated_at,
            deleted_at: note.deleted_at,
            revision: note.revision,
        })
        .collect())
}

pub async fn permanently_delete_note(
    id: &str,
    expected_revision: i64,
) -> Result<(), NoteMutationApiError> {
    let resp = Request::delete(&mutation_url(
        &format!("/api/trash/{id}"),
        expected_revision,
    ))
    .send()
    .await
    .map_err(|_| NoteMutationApiError::transport())?;
    ok_or_mutation_error(resp).await.map(|_| ())
}

fn restore_notes_body(notes: &[DeletedNoteSummary]) -> Value {
    let notes = notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "id": note.id,
                "expected_revision": note.revision,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "notes": notes })
}

pub async fn restore_deleted_notes(
    notes: &[DeletedNoteSummary],
) -> Result<(), NoteMutationApiError> {
    let resp = Request::post("/api/trash/restore")
        .json(&restore_notes_body(notes))
        .map_err(|_| NoteMutationApiError::transport())?
        .send()
        .await
        .map_err(|_| NoteMutationApiError::transport())?;
    ok_or_mutation_error(resp).await.map(|_| ())
}

// ---- Labels ----

#[derive(Deserialize)]
struct LabelKeyDto {
    key: String,
    description: String,
    value_type: String,
}

pub async fn list_labels() -> Result<Vec<LabelKey>, String> {
    let resp = Request::get("/api/labels")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let dtos: Vec<LabelKeyDto> = ok_or_body_error(resp)
        .await?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(dtos
        .into_iter()
        .map(|d| LabelKey {
            key: d.key,
            description: d.description,
            value_type: d.value_type,
        })
        .collect())
}

pub async fn create_label(key: &str, description: &str, value_type: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "key": key,
        "description": description,
        "value_type": value_type
    });
    let resp = Request::post("/api/labels")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
}

pub async fn update_label(key: &str, description: &str, value_type: &str) -> Result<(), String> {
    let body = serde_json::json!({ "description": description, "value_type": value_type });
    let resp = Request::put(&format!("/api/labels/{key}"))
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
}

pub async fn delete_label(key: &str) -> Result<(), String> {
    let resp = Request::delete(&format!("/api/labels/{key}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(content: AttachmentContent) -> NoteAttachment {
        NoteAttachment {
            id: "file-1".to_string(),
            path: "./file.dat".to_string(),
            mime: "application/octet-stream".to_string(),
            description: String::new(),
            content,
        }
    }

    #[test]
    fn attachment_requests_send_exactly_one_content_representation() {
        let text = attachment(AttachmentContent::Text("hello".to_string()));
        let text = serde_json::to_value(NoteAttachmentRequestDto::from(&text)).unwrap();
        assert_eq!(text["content"], "hello");
        assert!(text.get("content_base64").is_none());

        let binary = attachment(AttachmentContent::Base64("/wA=".to_string()));
        let binary = serde_json::to_value(NoteAttachmentRequestDto::from(&binary)).unwrap();
        assert_eq!(binary["content_base64"], "/wA=");
        assert!(binary.get("content").is_none());
    }

    #[test]
    fn attachment_metadata_responses_do_not_require_inline_content() {
        let metadata: NoteAttachmentMetadataDto = serde_json::from_value(serde_json::json!({
            "id": "file-1",
            "path": "./report.txt",
            "mime": "text/plain",
            "description": "Quarterly report",
        }))
        .unwrap();

        assert_eq!(metadata.id, "file-1");
        assert_eq!(metadata.path, "./report.txt");
        assert_eq!(metadata.mime, "text/plain");
        assert_eq!(metadata.description, "Quarterly report");
    }

    #[test]
    fn attachment_content_from_utf8_bytes_is_text() {
        let content = attachment_content_from_bytes(b"hello".to_vec());

        assert_eq!(content, AttachmentContent::Text("hello".to_string()));
    }

    #[test]
    fn attachment_content_from_invalid_utf8_bytes_is_base64() {
        let content = attachment_content_from_bytes(vec![0xff, 0x00]);

        assert_eq!(content, AttachmentContent::Base64("/wA=".to_string()));
    }

    #[test]
    fn attachment_urls_encode_each_path_segment() {
        assert_eq!(
            attachment_url(
                "/api/notes/note-1/attachments/",
                "./images/report #1?progress=50%.png"
            ),
            "/api/notes/note-1/attachments/images/report%20%231%3Fprogress%3D50%25.png"
        );
    }

    #[test]
    fn ordinary_note_mutations_include_the_loaded_revision() {
        let update = update_note_body("Title", "Body", &[], &[], 7);
        assert_eq!(update["expected_revision"], 7);
        assert_eq!(
            mutation_url("/api/notes/note-1", 7),
            "/api/notes/note-1?expected_revision=7"
        );

        let notes = vec![DeletedNoteSummary {
            id: "note-1".into(),
            title: "Title".into(),
            labels: vec![],
            created_at: 0,
            updated_at: 0,
            deleted_at: 0,
            revision: 8,
        }];
        assert_eq!(
            restore_notes_body(&notes),
            serde_json::json!({"notes": [{"id": "note-1", "expected_revision": 8}]})
        );
    }

    #[test]
    fn structured_stale_revision_errors_are_decoded_without_losing_details() {
        let error = decode_mutation_error(
            409,
            r#"{"code":"stale_revision","message":"the note changed after it was read","details":{"note_id":"note-1","expected_revision":5,"current_revision":6},"retryable":false}"#,
        );

        assert!(error.is_stale_revision());
        assert_eq!(error.details["expected_revision"], 5);
        assert_eq!(error.details["current_revision"], 6);
        assert_eq!(error.status, Some(409));
        assert!(!error.retryable);
    }

    #[test]
    fn ordinary_note_reads_require_the_authoritative_revision() {
        let note = serde_json::from_value::<NoteDto>(serde_json::json!({
            "id": "note-1",
            "title": "Title",
            "content": "Body",
            "attachments": [],
            "labels": [],
            "created_at": 1,
            "updated_at": 1
        }));

        assert!(note.is_err());
    }

    #[test]
    fn malformed_error_responses_use_safe_fallback_copy() {
        let error = decode_mutation_error(502, "proxy secret must not reach the UI");

        assert_eq!(error.code, "unexpected_response");
        assert!(error.retryable);
        assert!(!error.to_string().contains("proxy secret"));
    }
}
