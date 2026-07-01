use note_embedding::Embedder;
use note_storage::Storage;
use std::sync::Arc;

/// One struct holding a pooled DB handle and the shared embedder instance (docs/design.md §6).
/// Constructed once per process (Axum) or per stdio invocation — passed explicitly, no globals.
pub struct Context {
    pub storage: Arc<Storage>,
    pub embedder: Arc<dyn Embedder>,
}

impl Context {
    pub fn new(storage: Arc<Storage>, embedder: Arc<dyn Embedder>) -> Self {
        Self { storage, embedder }
    }
}
