use crate::state::{
    LabelFilter, LabelKey, NoteAttachment, NoteSummary, SearchResultSummary, SystemConfig,
    SystemInfo,
};
use gloo_net::http::{Request, Response};
use serde::Deserialize;

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

#[derive(Deserialize)]
struct NoteDto {
    id: String,
    title: String,
    content: String,
    #[serde(default)]
    attachments: Vec<NoteAttachment>,
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
pub struct DashboardSummary {
    pub note_count: usize,
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
    Ok(NoteSummary {
        id: d.id,
        title: d.title,
        content: d.content,
        attachments: d.attachments,
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
