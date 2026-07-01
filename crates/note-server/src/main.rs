mod labels_api;
mod notes_api;

use axum::Router;
use note_embedding::{BoundedEmbedder, StubEmbedder}; // swap StubEmbedder for OrtEmbedder once model weights are available
use note_pipelines::Context;
use note_storage::Storage;
use std::sync::Arc;

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
        let embedder = BoundedEmbedder::new(StubEmbedder, 4);
        let ctx = Arc::new(Context::new(Arc::new(storage), Arc::new(embedder)));

        // notes_router()/labels_router() are Router<Arc<Context>> — applying .with_state converts them
        // to Router<()>, which can then merge with mcp_router() (already Router<()>, self-stated).
        let rest = Router::new()
            .merge(notes_api::notes_router())
            .merge(labels_api::labels_router())
            .with_state(ctx.clone());
        let app: Router = rest.merge(note_mcp::mcp_router(ctx));

        let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
        axum::serve(listener, app).await?;
    }
    Ok(())
}
