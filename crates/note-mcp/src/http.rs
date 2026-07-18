//! MCP server exposing the two note tools over rmcp's Streamable HTTP transport.
//!
//! This reuses the transport-independent [`NoteMcpServer`] from [`crate::stdio`]
//! verbatim — the tools and server logic live there; here we only wrap that
//! server in rmcp's [`StreamableHttpService`] and expose it as an Axum
//! [`Router`] nested at `/mcp`. A single `/mcp` endpoint handles POST + GET with
//! an optional per-response SSE upgrade (docs/design.md §8); the legacy
//! two-endpoint HTTP+SSE transport is intentionally not implemented.
//!
//! note-server (Task 23) merges the returned router into its Axum app alongside
//! the REST routes.

use std::sync::Arc;

use axum::Router;
use note_pipelines::Context;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

use crate::NoteMcpServer;

/// Build an Axum [`Router`] serving the MCP protocol over Streamable HTTP at
/// `/mcp`, backed by a fresh [`NoteMcpServer`] per session built from `ctx`.
///
/// The service factory clones the shared [`Context`] (cheap — it is an `Arc`)
/// into a new `NoteMcpServer` for each session, so all sessions share the same
/// underlying storage and embedder while remaining independent MCP connections.
///
/// Runs the transport in **stateless mode** (`with_stateful_mode(false)`): every POST
/// is a self-contained request/response, so clients can call `tools/list`/`tools/call`
/// without first performing the `initialize` handshake and carrying an `Mcp-Session-Id`
/// header on every follow-up. In the default stateful mode, a first POST that isn't an
/// `initialize` request is rejected with `HTTP 422 "Unexpected message, expect initialize
/// request"` — which is what non-session-tracking clients hit. Our two tools are plain
/// request/response with no server-initiated streaming, so sessions buy us nothing here.
/// `with_json_response(true)` returns `application/json` directly instead of an SSE stream,
/// dropping the framing overhead (allowed by the MCP Streamable HTTP spec, 2025-06-18).
///
/// `disable_allowed_hosts()` clears rmcp's default DNS-rebinding-protection `Host` allowlist
/// (loopback only: `localhost`, `127.0.0.1`, `::1`), which otherwise `403`s any other `Host`.
/// This app is meant to be served behind a reverse proxy (e.g. Caddy at `notes.web-dev.zdns.cn`)
/// that forwards a real public `Host` and owns access control / TLS; keeping the allowlist here
/// would reject every proxied `/mcp` request. Host/origin gating is delegated to the proxy.
pub fn mcp_router(ctx: Arc<Context>) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .disable_allowed_hosts();
    let service = StreamableHttpService::new(
        move || Ok(NoteMcpServer::new(ctx.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new().nest_service("/mcp", service)
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    use tempfile::TempDir;

    // Returns the TempDir guard alongside the Context so the caller keeps it
    // alive: dropping it deletes the DB directory (mirrors stdio.rs tests).
    async fn test_context() -> (Context, Arc<dyn StorageBackend>, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let ctx = Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        );
        (ctx, backend, dir)
    }

    #[tokio::test]
    async fn mcp_router_builds() {
        let (ctx, _backend, _dir) = test_context().await;
        // Building the router must not panic; the type asserts it is an axum::Router.
        let _router: Router = mcp_router(Arc::new(ctx));
    }
}
