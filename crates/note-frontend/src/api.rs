use crate::state::{
    AttachmentContent, DeletedNoteSummary, LabelFilter, LabelKey, NoteAttachment, NoteSummary,
    SearchResultSummary, SystemConfig, SystemInfo,
};
use gloo_net::http::{Request, Response};
use serde::{Deserialize, Serialize};

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
struct NoteAttachmentResponseDto {
    id: String,
    path: String,
    mime: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    content_base64: Option<String>,
}

impl TryFrom<NoteAttachmentResponseDto> for NoteAttachment {
    type Error = String;

    fn try_from(attachment: NoteAttachmentResponseDto) -> Result<Self, Self::Error> {
        let content = match (attachment.content, attachment.content_base64) {
            (Some(content), _) => AttachmentContent::Text(content),
            (None, Some(content)) => AttachmentContent::Base64(content),
            (None, None) => {
                return Err(format!(
                    "attachment {} has no content representation",
                    attachment.id
                ));
            }
        };
        Ok(Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
            content,
        })
    }
}

#[derive(Deserialize)]
struct NoteDto {
    id: String,
    title: String,
    content: String,
    #[serde(default)]
    attachments: Vec<NoteAttachmentResponseDto>,
    labels: Vec<(String, String)>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    updated_at: i64,
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
    let attachments = d
        .attachments
        .into_iter()
        .map(NoteAttachment::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NoteSummary {
        id: d.id,
        title: d.title,
        content: d.content,
        attachments,
        labels: d.labels,
        created_at: d.created_at,
        updated_at: d.updated_at,
    })
}

pub async fn update_note(
    id: &str,
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    labels: &[(String, String)],
) -> Result<(), String> {
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
    let resp = Request::put(&format!("/api/notes/{id}"))
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
}

pub async fn delete_note(id: &str) -> Result<(), String> {
    let resp = Request::delete(&format!("/api/notes/{id}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
}

#[derive(Deserialize)]
struct TrashNoteDto {
    id: String,
    title: String,
    labels: Vec<(String, String)>,
    created_at: i64,
    updated_at: i64,
    deleted_at: i64,
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
        })
        .collect())
}

pub async fn permanently_delete_note(id: &str) -> Result<(), String> {
    let resp = Request::delete(&format!("/api/trash/{id}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
}

pub async fn restore_deleted_notes(ids: &[String]) -> Result<(), String> {
    let resp = Request::post("/api/trash/restore")
        .json(&serde_json::json!({ "ids": ids }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_body_error(resp).await.map(|_| ())
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
}
