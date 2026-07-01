//! MCP server exposing the two note tools over a stdio transport.
//!
//! JSON-RPC is spoken on stdin/stdout; rmcp keeps stdout clean for protocol
//! traffic, so anything we want logged must go to stderr (docs/design.md §8).
//! Both this transport and the Streamable HTTP transport (Task 20) delegate to
//! the same plain-async wrappers in [`crate::tools`] — no MCP-specific logic
//! lives here beyond request/response marshalling.

use std::sync::Arc;

use note_pipelines::Context;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Json, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::io::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tools::{
    save_note_tool, semantic_search_tool, SaveNoteToolInput, SaveNoteToolOutput,
    SemanticSearchToolInput, SemanticSearchToolResult,
};

/// MCP request schema for `save_note`. Mirrors [`SaveNoteToolInput`] but derives
/// [`JsonSchema`] so rmcp can advertise the tool's input schema over the wire.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveNoteRequest {
    /// Note title.
    pub title: String,
    /// Note body.
    pub content: String,
    /// Existing label keys to attach, as `(key, value)` pairs. Cannot create new
    /// label keys — that is REST/UI-only (docs/design.md §8).
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

/// MCP response schema for `save_note`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SaveNoteResponse {
    /// Id of the newly saved note.
    pub id: String,
}

// These `From` impls are the single place that names every field of the tool-layer twins, so
// adding a field to a `crate::tools` type forces a compile-time decision here rather than silently
// failing to expose it over MCP (the schema types intentionally stay separate to keep the
// transport-agnostic wrapper layer free of the schemars/JsonSchema dependency).
impl From<SaveNoteRequest> for SaveNoteToolInput {
    fn from(r: SaveNoteRequest) -> Self {
        Self {
            title: r.title,
            content: r.content,
            labels: r.labels,
        }
    }
}

impl From<SaveNoteToolOutput> for SaveNoteResponse {
    fn from(o: SaveNoteToolOutput) -> Self {
        Self { id: o.id }
    }
}

/// MCP request schema for `semantic_search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SemanticSearchRequest {
    /// Natural-language query.
    pub query: String,
    /// Maximum number of results to return.
    pub limit: usize,
}

/// A single `semantic_search` hit.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SemanticSearchHit {
    /// Id of the matching note.
    pub id: String,
    /// Title of the matching note.
    pub title: String,
    /// Relevance score.
    pub score: f32,
}

/// MCP response schema for `semantic_search`. Wraps the hits in an object so the
/// tool's output schema has an `object` root, as the MCP spec requires.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SemanticSearchResponse {
    /// Matching notes, best first.
    pub results: Vec<SemanticSearchHit>,
}

impl From<SemanticSearchRequest> for SemanticSearchToolInput {
    fn from(r: SemanticSearchRequest) -> Self {
        Self {
            query: r.query,
            limit: r.limit,
        }
    }
}

impl From<SemanticSearchToolResult> for SemanticSearchHit {
    fn from(r: SemanticSearchToolResult) -> Self {
        Self {
            id: r.id,
            title: r.title,
            score: r.score,
        }
    }
}

/// MCP server holding the shared pipeline context. Cloned per request by rmcp's
/// router, so state lives behind an `Arc`.
#[derive(Clone)]
pub struct NoteMcpServer {
    ctx: Arc<Context>,
    tool_router: ToolRouter<Self>,
}

impl NoteMcpServer {
    /// Build a server around a shared [`Context`].
    pub fn new(ctx: Arc<Context>) -> Self {
        Self {
            ctx,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl NoteMcpServer {
    /// Save a note with a title, body, and optional existing label keys.
    #[tool(name = "save_note", description = "Save a note with a title, content, and optional labels.")]
    pub async fn save_note(
        &self,
        params: Parameters<SaveNoteRequest>,
    ) -> Result<Json<SaveNoteResponse>, ErrorData> {
        let output = save_note_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_error_data)?;
        Ok(Json(output.into()))
    }

    /// Semantic (hybrid dense+sparse) search over saved notes.
    #[tool(
        name = "semantic_search",
        description = "Search saved notes semantically and return the top matches."
    )]
    pub async fn semantic_search(
        &self,
        params: Parameters<SemanticSearchRequest>,
    ) -> Result<Json<SemanticSearchResponse>, ErrorData> {
        let results = semantic_search_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_error_data)?;
        let hits = results.into_iter().map(Into::into).collect();
        Ok(Json(SemanticSearchResponse { results: hits }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NoteMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.instructions =
            Some("Note server exposing save_note and semantic_search over MCP.".to_string());
        info
    }
}

/// Map a pipeline/anyhow error into an MCP JSON-RPC internal error.
fn to_error_data(err: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(err.to_string(), None)
}

/// Serve the two note tools as an MCP server over stdio until the client
/// disconnects. JSON-RPC flows on stdin/stdout; keep logs on stderr.
pub async fn run_stdio(ctx: Context) -> anyhow::Result<()> {
    let server = NoteMcpServer::new(Arc::new(ctx));
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_embedding::StubEmbedder;
    use note_storage::Storage;
    use tempfile::TempDir;

    // Returns the TempDir guard alongside the Context so the caller keeps it
    // alive: dropping it deletes the DB directory (mirrors note-pipelines tests).
    async fn test_context() -> (Context, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let ctx = Context::new(Arc::new(storage), Arc::new(StubEmbedder));
        (ctx, dir)
    }

    #[tokio::test]
    async fn server_builds_and_lists_both_tools() {
        let (ctx, _dir) = test_context().await;
        let server = NoteMcpServer::new(Arc::new(ctx));
        let tools = server.tool_router.list_all();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"save_note"), "save_note missing: {names:?}");
        assert!(
            names.contains(&"semantic_search"),
            "semantic_search missing: {names:?}"
        );
    }
}
