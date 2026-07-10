//! MCP server exposing the note tools over a stdio transport.
//!
//! JSON-RPC is spoken on stdin/stdout; rmcp keeps stdout clean for protocol
//! traffic, so anything we want logged must go to stderr (docs/design.md §8).
//! Both this transport and the Streamable HTTP transport (Task 20) delegate to
//! the same plain-async wrappers in [`crate::tools`] — no MCP-specific logic
//! lives here beyond request/response marshalling. The stdio tool surface
//! includes whole-note tools plus `read_note_lines`/`edit_note` for tagged,
//! line-anchored body editing.

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
    delete_note_tool, edit_note_tool, get_note_tool, list_notes_tool, read_note_lines_tool,
    save_note_tool, semantic_search_tool, update_note_tool, LabelData, NoteData, NoteLine,
    NoteLinesData, SaveNoteToolInput, SaveNoteToolOutput, SemanticSearchToolInput,
    SemanticSearchToolResult, UpdateNoteToolInput,
};

/// MCP request schema for `save_note`. Mirrors [`SaveNoteToolInput`] but derives
/// [`JsonSchema`] so rmcp can advertise the tool's input schema over the wire.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveNoteRequest {
    /// Note title.
    pub title: String,
    /// Note body, formatted as Markdown.
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

/// MCP request schema for `get_note`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetNoteRequest {
    /// Note id.
    pub id: String,
}

/// MCP label schema embedded in note responses.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LabelSchema {
    /// Label key.
    pub key: String,
    /// Label value attached to the note.
    pub value: String,
    /// Label key description.
    pub description: String,
    /// Label value type used for comparisons.
    pub value_type: String,
}

/// MCP response schema for note-returning tools.
#[derive(Debug, Serialize, JsonSchema)]
pub struct NoteResponse {
    /// Note id.
    pub id: String,
    /// Note title.
    pub title: String,
    /// Note body, formatted as Markdown.
    pub content: String,
    /// Labels attached to the note.
    pub labels: Vec<LabelSchema>,
    /// Unix timestamp when the note was created.
    pub created_at: i64,
    /// Unix timestamp when the note was last updated.
    pub updated_at: i64,
}

impl From<LabelData> for LabelSchema {
    fn from(l: LabelData) -> Self {
        Self {
            key: l.key,
            value: l.value,
            description: l.description,
            value_type: l.value_type,
        }
    }
}

impl From<NoteData> for NoteResponse {
    fn from(n: NoteData) -> Self {
        Self {
            id: n.id,
            title: n.title,
            content: n.content,
            labels: n.labels.into_iter().map(Into::into).collect(),
            created_at: n.created_at,
            updated_at: n.updated_at,
        }
    }
}

/// MCP request schema for `read_note_lines`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadNoteLinesRequest {
    /// Note id.
    pub id: String,
}

/// One numbered line in a note body.
#[derive(Debug, Serialize, JsonSchema)]
pub struct NoteLineSchema {
    /// 1-indexed line number.
    pub n: usize,
    /// Line text without the line separator.
    pub text: String,
}

/// MCP response schema for line-editing reads and edits.
#[derive(Debug, Serialize, JsonSchema)]
pub struct NoteLinesResponse {
    /// Note id.
    pub id: String,
    /// Content hash tag used to guard subsequent `edit_note` calls.
    pub tag: String,
    /// Numbered note body lines.
    pub lines: Vec<NoteLineSchema>,
}

impl From<NoteLine> for NoteLineSchema {
    fn from(l: NoteLine) -> Self {
        Self {
            n: l.n,
            text: l.text,
        }
    }
}

impl From<NoteLinesData> for NoteLinesResponse {
    fn from(n: NoteLinesData) -> Self {
        Self {
            id: n.id,
            tag: n.tag,
            lines: n.lines.into_iter().map(Into::into).collect(),
        }
    }
}

/// MCP edit operation schema for `edit_note`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOpSchema {
    /// Replace an original line range with zero or more lines.
    Swap {
        /// First original line to replace, 1-indexed.
        from: usize,
        /// Last original line to replace, inclusive.
        to: usize,
        /// Replacement lines.
        lines: Vec<String>,
    },
    /// Delete an original line range.
    Delete {
        /// First original line to delete, 1-indexed.
        from: usize,
        /// Last original line to delete, inclusive.
        to: usize,
    },
    /// Insert lines before an original line.
    InsertBefore {
        /// Original anchor line, 1-indexed.
        line: usize,
        /// Lines to insert.
        lines: Vec<String>,
    },
    /// Insert lines after an original line.
    InsertAfter {
        /// Original anchor line, 1-indexed.
        line: usize,
        /// Lines to insert.
        lines: Vec<String>,
    },
    /// Insert lines at the start of the note body.
    InsertHead {
        /// Lines to insert.
        lines: Vec<String>,
    },
    /// Insert lines at the end of the note body.
    InsertTail {
        /// Lines to insert.
        lines: Vec<String>,
    },
}

impl From<EditOpSchema> for note_pipelines::EditOp {
    fn from(op: EditOpSchema) -> Self {
        match op {
            EditOpSchema::Swap { from, to, lines } => Self::Swap { from, to, lines },
            EditOpSchema::Delete { from, to } => Self::Delete { from, to },
            EditOpSchema::InsertBefore { line, lines } => Self::InsertBefore { line, lines },
            EditOpSchema::InsertAfter { line, lines } => Self::InsertAfter { line, lines },
            EditOpSchema::InsertHead { lines } => Self::InsertHead { lines },
            EditOpSchema::InsertTail { lines } => Self::InsertTail { lines },
        }
    }
}

/// MCP request schema for `edit_note`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditNoteRequest {
    /// Note id.
    pub id: String,
    /// Tag from `read_note_lines`.
    pub tag: String,
    /// Line edit operations, anchored to the ORIGINAL line numbers.
    pub edits: Vec<EditOpSchema>,
}

/// MCP request schema for `update_note`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateNoteRequest {
    /// Note id.
    pub id: String,
    /// New note title.
    pub title: String,
    /// New note body, formatted as Markdown.
    pub content: String,
    /// Existing label keys to attach, as `(key, value)` pairs. Cannot create new
    /// label keys — that is REST/UI-only (docs/design.md §8).
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

impl From<UpdateNoteRequest> for UpdateNoteToolInput {
    fn from(r: UpdateNoteRequest) -> Self {
        Self {
            id: r.id,
            title: r.title,
            content: r.content,
            labels: r.labels,
        }
    }
}

/// MCP request schema for `delete_note`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteNoteRequest {
    /// Note id.
    pub id: String,
}

/// MCP response schema for `delete_note`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DeleteNoteResponse {
    /// Whether a note was deleted.
    pub deleted: bool,
}

/// MCP request schema for `list_notes`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNotesRequest {
    /// Max notes to return.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Number of notes to skip.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Label selector, `&`-separated terms ANDed: `key=value`, `key>=value`, or bare `key` (any value). e.g. `env=prod&version>=1.2.0`.
    #[serde(default)]
    pub label: Option<String>,
}

/// MCP response schema for `list_notes`. Wraps the notes in an object so the
/// tool's output schema has an `object` root, as the MCP spec requires.
#[derive(Debug, Serialize, JsonSchema)]
pub struct NoteListResponse {
    /// Saved notes.
    pub notes: Vec<NoteResponse>,
}

/// MCP request schema for `semantic_search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SemanticSearchRequest {
    /// Natural-language query.
    pub query: String,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Label selector, `&`-separated terms ANDed: `key=value`, `key>=value`, or bare `key` (any value). e.g. `env=prod&version>=1.2.0`.
    #[serde(default)]
    pub label: Option<String>,
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
            label: r.label,
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
    #[tool(
        name = "save_note",
        description = "Save a note with a title, Markdown content, and optional labels."
    )]
    pub async fn save_note(
        &self,
        params: Parameters<SaveNoteRequest>,
    ) -> Result<Json<SaveNoteResponse>, ErrorData> {
        let output = save_note_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_error_data)?;
        Ok(Json(output.into()))
    }

    /// Fetch a single note by id.
    #[tool(name = "get_note", description = "Fetch a single note by id.")]
    pub async fn get_note(
        &self,
        params: Parameters<GetNoteRequest>,
    ) -> Result<Json<NoteResponse>, ErrorData> {
        let id = params.0.id;
        let output = get_note_tool(&self.ctx, &id).await.map_err(to_error_data)?;
        let note =
            output.ok_or_else(|| to_error_data(anyhow::anyhow!("note not found: {}", id)))?;
        Ok(Json(note.into()))
    }

    /// Read a note's body as numbered lines with a content tag.
    #[tool(
        name = "read_note_lines",
        description = "Read a note's Markdown body as numbered lines with a content tag for editing."
    )]
    pub async fn read_note_lines(
        &self,
        params: Parameters<ReadNoteLinesRequest>,
    ) -> Result<Json<NoteLinesResponse>, ErrorData> {
        let id = params.0.id;
        let output = read_note_lines_tool(&self.ctx, &id)
            .await
            .map_err(to_error_data)?;
        let note =
            output.ok_or_else(|| to_error_data(anyhow::anyhow!("note not found: {}", id)))?;
        Ok(Json(note.into()))
    }

    /// Edit a note's body with line-range operations guarded by a content tag.
    #[tool(
        name = "edit_note",
        description = "Edit a note's Markdown body with line-range operations (swap/delete/insert) anchored by a content tag; rejects if the note changed since it was read."
    )]
    pub async fn edit_note(
        &self,
        params: Parameters<EditNoteRequest>,
    ) -> Result<Json<NoteLinesResponse>, ErrorData> {
        let req = params.0;
        let id = req.id;
        let edits = req.edits.into_iter().map(Into::into).collect();
        let output = edit_note_tool(&self.ctx, &id, &req.tag, edits)
            .await
            .map_err(to_error_data)?;
        let note =
            output.ok_or_else(|| to_error_data(anyhow::anyhow!("note not found: {}", id)))?;
        Ok(Json(note.into()))
    }

    /// Update an existing note's title, body, and labels.
    #[tool(
        name = "update_note",
        description = "Update an existing note's title, Markdown content, and labels by id."
    )]
    pub async fn update_note(
        &self,
        params: Parameters<UpdateNoteRequest>,
    ) -> Result<Json<NoteResponse>, ErrorData> {
        let id = params.0.id.clone();
        let output = update_note_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_error_data)?;
        let note =
            output.ok_or_else(|| to_error_data(anyhow::anyhow!("note not found: {}", id)))?;
        Ok(Json(note.into()))
    }

    /// Delete a note by id.
    #[tool(name = "delete_note", description = "Delete a note by id.")]
    pub async fn delete_note(
        &self,
        params: Parameters<DeleteNoteRequest>,
    ) -> Result<Json<DeleteNoteResponse>, ErrorData> {
        let deleted = delete_note_tool(&self.ctx, &params.0.id)
            .await
            .map_err(to_error_data)?;
        Ok(Json(DeleteNoteResponse { deleted }))
    }

    /// List saved notes.
    #[tool(
        name = "list_notes",
        description = "List saved notes with optional limit, offset, and label selector filters."
    )]
    pub async fn list_notes(
        &self,
        params: Parameters<ListNotesRequest>,
    ) -> Result<Json<NoteListResponse>, ErrorData> {
        let req = params.0;
        let notes = list_notes_tool(
            &self.ctx,
            req.limit.map(i64::from),
            req.offset.map(i64::from),
            req.label,
        )
        .await
        .map_err(to_error_data)?;
        Ok(Json(NoteListResponse {
            notes: notes.into_iter().map(Into::into).collect(),
        }))
    }

    /// Semantic (hybrid dense+sparse) search over saved notes.
    #[tool(
        name = "semantic_search",
        description = "Search saved notes semantically with an optional label selector filter and return the top matches."
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
        info.instructions = Some(
            "Note server exposing save_note, get_note, read_note_lines, edit_note, update_note, delete_note, list_notes, and semantic_search over MCP."
                .to_string(),
        );
        info
    }
}

/// Map a pipeline/anyhow error into an MCP JSON-RPC internal error.
fn to_error_data(err: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(err.to_string(), None)
}

/// Serve the note tools as an MCP server over stdio until the client
/// disconnects. JSON-RPC flows on stdin/stdout; keep logs on stderr.
pub async fn run_stdio(ctx: Arc<Context>) -> anyhow::Result<()> {
    let server = NoteMcpServer::new(ctx);
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
    async fn server_builds_and_lists_all_tools() {
        let (ctx, _dir) = test_context().await;
        let server = NoteMcpServer::new(Arc::new(ctx));
        let tools = server.tool_router.list_all();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"save_note"), "save_note missing: {names:?}");
        assert!(names.contains(&"get_note"), "get_note missing: {names:?}");
        assert!(
            names.contains(&"read_note_lines"),
            "read_note_lines missing: {names:?}"
        );
        assert!(names.contains(&"edit_note"), "edit_note missing: {names:?}");
        assert!(
            names.contains(&"update_note"),
            "update_note missing: {names:?}"
        );
        assert!(
            names.contains(&"delete_note"),
            "delete_note missing: {names:?}"
        );
        assert!(
            names.contains(&"list_notes"),
            "list_notes missing: {names:?}"
        );
        assert!(
            names.contains(&"semantic_search"),
            "semantic_search missing: {names:?}"
        );
    }
}
