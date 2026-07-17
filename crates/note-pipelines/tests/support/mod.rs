use note_embedding::StubEmbedder;
use note_pipelines::Context;
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use std::sync::Arc;

pub async fn test_context() -> (Context, Arc<dyn StorageBackend>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        dir.path().join("attachments"),
    );
    (ctx, backend, dir)
}
