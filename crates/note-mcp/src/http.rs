//! MCP server exposing Note and Org tools over rmcp's Streamable HTTP transport.
//!
//! This reuses the transport-independent [`NoteMcpServer`] from [`crate::server`]
//! verbatim — the tools and server logic live there; here we only wrap that
//! server in rmcp's [`StreamableHttpService`] and expose it as an Axum
//! [`Router`] nested at `/mcp` and `/org/mcp`. Both endpoints are POST-only,
//! stateless, and return JSON for simple calls. They create no MCP sessions or
//! authentication layer; those deployment concerns belong to the front proxy.
//!
//! note-server (Task 23) merges the returned router into its Axum app alongside
//! the REST routes.

use std::sync::Arc;

use axum::Router;
use note_pipelines::{org::OrgContext, Context};
use rmcp::transport::streamable_http_server::{
    session::never::NeverSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

use crate::NoteMcpServer;

/// Build an Axum [`Router`] serving Note MCP at `/mcp` and Org MCP at
/// `/org/mcp`, each backed by a fresh [`NoteMcpServer`] per request.
///
/// The service factory clones the shared [`Context`] (cheap — it is an `Arc`)
/// into a new `NoteMcpServer` for each request, so all requests share the same
/// underlying storage and embedder without creating transport sessions.
///
/// 2026-07-28 requests are stateless by protocol. Earlier versions also use
/// stateless mode (`with_legacy_session_mode(false)`) so clients need not carry
/// an `Mcp-Session-Id` between calls.
/// `with_json_response(true)` returns `application/json` directly instead of an SSE stream,
/// dropping the framing overhead (allowed by the MCP Streamable HTTP spec, 2025-06-18).
///
/// `disable_allowed_hosts()` clears rmcp's default DNS-rebinding-protection `Host` allowlist
/// (loopback only: `localhost`, `127.0.0.1`, `::1`), which otherwise `403`s any other `Host`.
/// This app is meant to be served behind a reverse proxy (e.g. Caddy at `notes.web-dev.zdns.cn`)
/// that forwards a real public `Host` and owns access control / TLS; keeping the allowlist here
/// would reject proxied requests. Host gating is delegated to the proxy;
/// any present `Origin` is rejected by the empty Origin allowlist.
pub fn mcp_router(ctx: Arc<Context>, org_ctx: Arc<OrgContext>) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .disable_allowed_hosts()
        .enforce_origin_validation();
    let note_ctx = ctx.clone();
    let note_org_ctx = org_ctx.clone();
    let note_service = StreamableHttpService::new(
        move || {
            Ok(NoteMcpServer::notes_only(
                note_ctx.clone(),
                note_org_ctx.clone(),
            ))
        },
        Arc::new(NeverSessionManager::default()),
        config.clone(),
    );
    let org_service = StreamableHttpService::new(
        move || Ok(NoteMcpServer::org_only(ctx.clone(), org_ctx.clone())),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .nest_service("/mcp", note_service)
        .nest_service("/org/mcp", org_service)
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
    // alive: dropping it deletes the DB directory (mirrors server.rs tests).
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
        let (ctx, backend, _dir) = test_context().await;
        let org_ctx = Arc::new(OrgContext::new(
            backend,
            Arc::new(note_pipelines::org::SystemOrgClock),
        ));
        // Building the router must not panic; the type asserts it is an axum::Router.
        let _router: Router = mcp_router(Arc::new(ctx), org_ctx);
    }
}
