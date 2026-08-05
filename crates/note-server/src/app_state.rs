use std::sync::Arc;

use axum::extract::FromRef;
use note_pipelines::{org::OrgContext, Context};

#[derive(Clone)]
pub struct AppState {
    pub note: Arc<Context>,
    pub org: Arc<OrgContext>,
}

impl AppState {
    pub fn new(note: Arc<Context>, org: Arc<OrgContext>) -> Self {
        Self { note, org }
    }
}

impl FromRef<AppState> for Arc<Context> {
    fn from_ref(state: &AppState) -> Self {
        state.note.clone()
    }
}

impl FromRef<AppState> for Arc<OrgContext> {
    fn from_ref(state: &AppState) -> Self {
        state.org.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_pipelines::org::SystemOrgClock;
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;

    #[tokio::test]
    async fn app_state_from_ref_preserves_both_context_identities() {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("notes.db"))
                .await
                .unwrap(),
        );
        let note = Arc::new(Context::new(
            storage.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        let org = Arc::new(OrgContext::new(storage, Arc::new(SystemOrgClock)));
        let state = AppState::new(note.clone(), org.clone());

        let extracted_note = <Arc<Context>>::from_ref(&state);
        let extracted_org = <Arc<OrgContext>>::from_ref(&state);

        assert!(Arc::ptr_eq(&extracted_note, &note));
        assert!(Arc::ptr_eq(&extracted_org, &org));
    }
}
