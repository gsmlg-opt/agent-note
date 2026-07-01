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
pub fn mcp_router(ctx: Arc<Context>) -> Router {
    let service = StreamableHttpService::new(
        move || Ok(NoteMcpServer::new(ctx.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    Router::new().nest_service("/mcp", service)
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_embedding::StubEmbedder;
    use note_storage::Storage;
    use tempfile::TempDir;

    // Returns the TempDir guard alongside the Context so the caller keeps it
    // alive: dropping it deletes the DB directory (mirrors stdio.rs tests).
    async fn test_context() -> (Context, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let ctx = Context::new(Arc::new(storage), Arc::new(StubEmbedder));
        (ctx, dir)
    }

    #[tokio::test]
    async fn mcp_router_builds() {
        let (ctx, _dir) = test_context().await;
        // Building the router must not panic; the type asserts it is an axum::Router.
        let _router: Router = mcp_router(Arc::new(ctx));
    }
}
