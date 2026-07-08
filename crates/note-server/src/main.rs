mod labels_api;
mod notes_api;

use axum::response::IntoResponse;
use axum::Router;
use note_embedding::{BoundedEmbedder, StubEmbedder};
use note_pipelines::Context;
use note_storage::Storage;
use std::sync::Arc;

/// Provisional cap on concurrent inference calls on the HTTP path. With `StubEmbedder` any value
/// works; once the real `OrtEmbedder` lands, align this with the ONNX session's thread count so
/// the semaphore actually bounds inference rather than guessing (docs/design.md §4/§5).
const HTTP_INFERENCE_CONCURRENCY: usize = 4;

fn build_embedder(concurrency: usize) -> anyhow::Result<Arc<dyn note_embedding::Embedder>> {
    match std::env::var("NOTE_MODEL_PATH") {
        Ok(p) if !p.is_empty() => {
            eprintln!("embedder: ONNX ({p})");
            let ort = note_embedding::OrtEmbedder::load(std::path::Path::new(&p))?;
            Ok(Arc::new(BoundedEmbedder::new(ort, concurrency)))
        }
        _ => {
            eprintln!("embedder: stub");
            Ok(Arc::new(BoundedEmbedder::new(StubEmbedder, concurrency)))
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let stdio_mode = std::env::args().any(|a| a == "--stdio");
    let db_path = std::env::var("NOTE_DB_PATH").unwrap_or_else(|_| "notes.db".to_string());

    if stdio_mode {
        // stdio is single-client — no connection pool needed (docs/design.md §9).
        let storage = Storage::open_local(&db_path).await?;
        let embedder = build_embedder(1)?;
        let ctx = Context::new(Arc::new(storage), embedder);
        let n = note_pipelines::backfill_chunk_embeddings(&ctx).await?;
        if n > 0 {
            eprintln!("backfilled chunk embeddings for {n} notes");
        }
        note_mcp::run_stdio(ctx).await?;
    } else {
        // Axum is the only process that needs connection pooling (docs/design.md §2).
        // TODO: size the pool deliberately once concurrency requirements are clearer (docs/design.md §9
        // open decision) — starting with a single shared Storage handle is a placeholder, not a final answer.
        let storage = Storage::open_local(&db_path).await?;
        let embedder = build_embedder(HTTP_INFERENCE_CONCURRENCY)?;
        let ctx = Arc::new(Context::new(Arc::new(storage), embedder));
        let n = note_pipelines::backfill_chunk_embeddings(&ctx).await?;
        if n > 0 {
            eprintln!("backfilled chunk embeddings for {n} notes");
        }

        // notes_router()/labels_router() are Router<Arc<Context>> — applying .with_state converts them
        // to Router<()>, which can then merge with mcp_router() (already Router<()>, self-stated).
        let rest = Router::new()
            .merge(notes_api::notes_router())
            .merge(labels_api::labels_router())
            .with_state(ctx.clone());
        let mut app: Router = rest.merge(note_mcp::mcp_router(ctx));

        // If NOTE_STATIC_DIR is set (e.g. the Docker image points it at the built wasm bundle),
        // serve those static files for any path the API/MCP routes don't claim, falling back to
        // index.html. In debug builds, `cargo run` also serves a previously built Trunk bundle.
        let static_dir = match std::env::var("NOTE_STATIC_DIR") {
            Ok(static_dir) if !static_dir.is_empty() => Some(static_dir),
            _ if cfg!(debug_assertions)
                && std::path::Path::new("crates/note-frontend/dist").is_dir() =>
            {
                Some("crates/note-frontend/dist".to_string())
            }
            _ => None,
        };
        if let Some(static_dir) = &static_dir {
            // SPA fallback for any path the API/MCP routes don't claim: serve a real static asset
            // when one exists at that path, otherwise return index.html (200) so client-side routes
            // like /new and /labels boot on a direct load or refresh. (ServeDir's not_found_service
            // would serve index.html but with a 404 status, wrong for a valid SPA route.)
            let static_dir = static_dir.clone();
            let index_html =
                std::fs::read_to_string(format!("{static_dir}/index.html")).unwrap_or_default();
            app = app.fallback(move |uri: axum::http::Uri| {
                let static_dir = static_dir.clone();
                let index_html = index_html.clone();
                async move {
                    let path = uri.path().trim_start_matches('/');
                    if !path.is_empty() && !path.contains("..") {
                        if let Ok(bytes) = tokio::fs::read(format!("{static_dir}/{path}")).await {
                            let content_type = match path.rsplit('.').next() {
                                Some("js") => "text/javascript",
                                Some("wasm") => "application/wasm",
                                Some("css") => "text/css",
                                Some("html") => "text/html; charset=utf-8",
                                Some("json") => "application/json",
                                Some("svg") => "image/svg+xml",
                                Some("ico") => "image/x-icon",
                                _ => "application/octet-stream",
                            };
                            return ([(axum::http::header::CONTENT_TYPE, content_type)], bytes)
                                .into_response();
                        }
                    }
                    axum::response::Html(index_html).into_response()
                }
            });
        }

        // Default to loopback: a fully-offline, unauthenticated personal app (docs/design.md §1),
        // and note-mcp's /mcp router defaults to a loopback-only Host allowlist. NOTE_BIND_ADDR
        // overrides it — the Docker image sets 0.0.0.0:6222 so the container is reachable via `-p`
        // (container-network isolation makes that safe; exposing it to your LAN is your `-p` choice).
        let bind_addr =
            std::env::var("NOTE_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:6222".to_string());
        eprintln!("note-server listening on http://{bind_addr}");
        if let Some(static_dir) = &static_dir {
            eprintln!("serving frontend from {static_dir}");
        } else {
            eprintln!(
                "API only — run `trunk build` then re-run for the UI, or `trunk serve` for hot reload"
            );
        }
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
        axum::serve(listener, app).await?;
    }
    Ok(())
}
