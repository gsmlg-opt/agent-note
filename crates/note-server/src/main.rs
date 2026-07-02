mod labels_api;
mod notes_api;

use axum::Router;
use note_embedding::{BoundedEmbedder, StubEmbedder}; // swap StubEmbedder for OrtEmbedder once model weights are available
use note_pipelines::Context;
use note_storage::Storage;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};

/// Provisional cap on concurrent inference calls on the HTTP path. With `StubEmbedder` any value
/// works; once the real `OrtEmbedder` lands, align this with the ONNX session's thread count so
/// the semaphore actually bounds inference rather than guessing (docs/design.md §4/§5).
const HTTP_INFERENCE_CONCURRENCY: usize = 4;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let stdio_mode = std::env::args().any(|a| a == "--stdio");
    let db_path = std::env::var("NOTE_DB_PATH").unwrap_or_else(|_| "notes.db".to_string());

    if stdio_mode {
        // stdio is single-client — no connection pool needed (docs/design.md §9).
        let storage = Storage::open_local(&db_path).await?;
        let embedder = BoundedEmbedder::new(StubEmbedder, 1);
        let ctx = Context::new(Arc::new(storage), Arc::new(embedder));
        note_mcp::run_stdio(ctx).await?;
    } else {
        // Axum is the only process that needs connection pooling (docs/design.md §2).
        // TODO: size the pool deliberately once concurrency requirements are clearer (docs/design.md §9
        // open decision) — starting with a single shared Storage handle is a placeholder, not a final answer.
        let storage = Storage::open_local(&db_path).await?;
        let embedder = BoundedEmbedder::new(StubEmbedder, HTTP_INFERENCE_CONCURRENCY);
        let ctx = Arc::new(Context::new(Arc::new(storage), Arc::new(embedder)));

        // notes_router()/labels_router() are Router<Arc<Context>> — applying .with_state converts them
        // to Router<()>, which can then merge with mcp_router() (already Router<()>, self-stated).
        let rest = Router::new()
            .merge(notes_api::notes_router())
            .merge(labels_api::labels_router())
            .with_state(ctx.clone());
        let mut app: Router = rest.merge(note_mcp::mcp_router(ctx));

        // If NOTE_STATIC_DIR is set (e.g. the Docker image points it at the built wasm bundle),
        // serve those static files for any path the API/MCP routes don't claim, falling back to
        // index.html. Unset in local dev, where `trunk serve` serves the frontend and proxies here.
        if let Ok(static_dir) = std::env::var("NOTE_STATIC_DIR") {
            if !static_dir.is_empty() {
                let index = ServeFile::new(format!("{static_dir}/index.html"));
                app = app.fallback_service(ServeDir::new(&static_dir).not_found_service(index));
            }
        }

        // Default to loopback: a fully-offline, unauthenticated personal app (docs/design.md §1),
        // and note-mcp's /mcp router defaults to a loopback-only Host allowlist. NOTE_BIND_ADDR
        // overrides it — the Docker image sets 0.0.0.0:8080 so the container is reachable via `-p`
        // (container-network isolation makes that safe; exposing it to your LAN is your `-p` choice).
        let bind_addr =
            std::env::var("NOTE_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
        axum::serve(listener, app).await?;
    }
    Ok(())
}
