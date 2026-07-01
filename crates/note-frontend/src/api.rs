use crate::state::SearchResultSummary;
use gloo_net::http::Request;
use serde::Deserialize;

#[derive(Deserialize)]
struct SearchResultDto {
    id: String,
    title: String,
    score: f32,
}

pub async fn search(query: &str, limit: usize) -> Result<Vec<SearchResultSummary>, gloo_net::Error> {
    let body = serde_json::json!({ "query": query, "limit": limit });
    // POST, not GET: the browser Fetch API forbids a body on GET requests, and search params go in
    // the JSON body. note-server's /api/notes/search is registered as POST for this reason.
    let dtos: Vec<SearchResultDto> = Request::post("/api/notes/search")
        .json(&body)?
        .send()
        .await?
        .json()
        .await?;
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
) -> Result<String, gloo_net::Error> {
    let body = serde_json::json!({ "title": title, "content": content, "labels": labels });
    #[derive(Deserialize)]
    struct SaveResp {
        id: String,
    }
    let resp: SaveResp = Request::post("/api/notes")
        .json(&body)?
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.id)
}
