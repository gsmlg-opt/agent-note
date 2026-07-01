use crate::state::SearchResultSummary;
use gloo_net::http::{Request, Response};
use serde::Deserialize;

#[derive(Deserialize)]
struct SearchResultDto {
    id: String,
    title: String,
    score: f32,
}

// gloo-net (like the Fetch API) does NOT return Err on a 4xx/5xx status — .send() resolves fine and
// only the body read would fail. So without this check, a server error (e.g. "empty title") gets
// deserialized as the success DTO, failing with a confusing serde error instead of the real message.
// This reads the plain-text error body note-server returns and surfaces it. Errors are Strings so the
// server's actual message reaches the UI error slot.
async fn ok_or_body_error(resp: Response) -> Result<Response, String> {
    if resp.ok() {
        Ok(resp)
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("server error {status}: {body}"))
    }
}

pub async fn search(query: &str, limit: usize) -> Result<Vec<SearchResultSummary>, String> {
    let body = serde_json::json!({ "query": query, "limit": limit });
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
    // Guard against a non-finite score from a malformed payload (would break PartialEq/render).
    Ok(dtos
        .into_iter()
        .map(|d| SearchResultSummary {
            id: d.id,
            title: d.title,
            score: if d.score.is_finite() { d.score } else { 0.0 },
        })
        .collect())
}

pub async fn save_note(
    title: &str,
    content: &str,
    labels: &[(String, String)],
) -> Result<String, String> {
    let body = serde_json::json!({ "title": title, "content": content, "labels": labels });
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
