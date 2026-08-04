use note_attachments::{
    AttachmentStore, AttachmentStoreInfo, PreparedAttachmentMutation, PreparedAttachmentSet,
};
use note_core::{LabelSelector, NoteAttachment};
use note_pipelines::EmbeddingJobNotifier;
use note_storage::{
    ActiveNoteSource, AttachmentMetadataUpdate, BackendInfo, CompareAndSwap, ConditionalUpdate,
    EmbeddingDashboardStatus, EmbeddingJob, EmbeddingRepository, LabelRepository, NewNote,
    NewOrgAttempt, NewOrgDocument, NewOrgEvent, NewOrgWorkspace, NoteChunk, NoteFieldsUpdate,
    NoteUpdate, NotesRepository, OrgAttempt, OrgAttemptUpdate, OrgDocument,
    OrgDocumentOwnershipMove, OrgDocumentOwnershipMoveResult, OrgDocumentUpdate, OrgEvent,
    OrgProjectedWorkItem, OrgRepository, OrgWorkspace, OrgWorkspaceUpdate, RetrievalRepository,
    SettingsRepository, StorageBackend, StorageError, StorageErrorKind, StorageResult,
    StorageSession, StorageTransaction, StoredOrgOperation, TransactionMode, UpsertNoteChunk,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Notify;

pub type EventLog = Arc<Mutex<Vec<String>>>;

pub fn event_log() -> EventLog {
    Arc::new(Mutex::new(Vec::new()))
}

pub struct EventStorageBackend {
    inner: Arc<dyn StorageBackend>,
    events: EventLog,
    fail_commit: Arc<AtomicBool>,
}

impl EventStorageBackend {
    pub fn new(inner: Arc<dyn StorageBackend>, events: EventLog) -> Self {
        Self {
            inner,
            events,
            fail_commit: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn fail_next_commit(&self) {
        self.fail_commit.store(true, Ordering::SeqCst);
    }
}

struct EventTransaction {
    inner: Box<dyn StorageTransaction>,
    events: EventLog,
    fail_commit: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl StorageBackend for EventStorageBackend {
    async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
        self.inner.session().await
    }

    async fn begin(&self, mode: TransactionMode) -> StorageResult<Box<dyn StorageTransaction>> {
        let transaction = self.inner.begin(mode).await?;
        self.events.lock().unwrap().push("begin".into());
        Ok(Box::new(EventTransaction {
            inner: transaction,
            events: self.events.clone(),
            fail_commit: self.fail_commit.clone(),
        }))
    }

    async fn info(&self) -> StorageResult<BackendInfo> {
        self.inner.info().await
    }
}

macro_rules! impl_forward_repository {
    ($repository:path { $(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $result:ty;)* }) => {
        #[async_trait::async_trait]
        impl $repository for EventTransaction {
            $(
                async fn $name(&self, $($arg: $ty),*) -> StorageResult<$result> {
                    self.inner.$name($($arg),*).await
                }
            )*
        }
    };
}

impl_forward_repository! {
    NotesRepository {
        fn insert_note(note: NewNote<'_>) -> ();
        fn get_note_revision(id: &str) -> Option<i64>;
        fn note_exists(id: &str) -> bool;
        fn get_note(id: &str) -> Option<note_core::Note>;
        fn get_note_content(id: &str) -> Option<String>;
        fn update_note(note: NoteUpdate<'_>) -> u64;
        fn update_note_fields(note: NoteFieldsUpdate<'_>) -> u64;
        fn update_note_attachments(note: AttachmentMetadataUpdate<'_>) -> u64;
        fn soft_delete_note(id: &str, deleted_at: i64) -> u64;
        fn get_deleted_note_content_and_revision(id: &str) -> Option<(String, i64)>;
        fn restore_note(id: &str, note_revision: i64) -> u64;
        fn permanently_delete_note(id: &str) -> u64;
        fn list_expired_deleted_note_ids(cutoff: i64) -> Vec<String>;
        fn clear_note_search_data(id: &str) -> ();
        fn clear_note_labels(id: &str) -> ();
        fn clear_note_chunk_derived(id: &str, chunk_idx: i64) -> ();
        fn clear_note_chunks_from_derived(id: &str, min_chunk_idx: i64) -> ();
        fn list_notes(
            selectors: &[LabelSelector],
            limit: Option<i64>,
            offset: Option<i64>,
        ) -> Vec<note_core::Note>;
        fn list_all_notes() -> Vec<note_core::Note>;
        fn list_note_summaries(
            selectors: &[LabelSelector],
            limit: Option<i64>,
            offset: Option<i64>,
        ) -> Vec<note_core::NoteListItem>;
        fn list_deleted_note_summaries() -> Vec<note_core::NoteListItem>;
        fn count_notes(selectors: &[LabelSelector]) -> usize;
        fn matching_note_ids(selectors: &[LabelSelector]) -> Vec<String>;
        fn list_active_note_sources() -> Vec<ActiveNoteSource>;
    }
}

impl_forward_repository! {
    LabelRepository {
        fn insert_label_key(key: &str, description: &str) -> ();
        fn insert_label_key_if_missing(key: &str, description: &str) -> ();
        fn insert_label_key_with_type(
            key: &str,
            description: &str,
            value_type: note_core::LabelValueType,
        ) -> ();
        fn list_label_keys() -> Vec<note_core::LabelKey>;
        fn update_label_key(key: &str, description: &str) -> ();
        fn update_label_key_with_type(
            key: &str,
            description: &str,
            value_type: note_core::LabelValueType,
        ) -> ();
        fn delete_label_key(key: &str) -> ();
        fn attach_label(note_id: &str, key: &str, value: &str) -> ();
        fn labels_for_note(note_id: &str) -> Vec<note_core::Label>;
        fn label_note_counts() -> Vec<(String, usize)>;
        fn find_note_with_labels(labels: &[(String, String)]) -> Option<String>;
    }
}

impl_forward_repository! {
    EmbeddingRepository {
        fn embedding_dashboard_status() -> EmbeddingDashboardStatus;
        fn list_note_chunks(note_id: &str) -> Vec<NoteChunk>;
        fn get_note_chunk(note_id: &str, chunk_idx: i64) -> Option<NoteChunk>;
        fn upsert_note_chunk(chunk: UpsertNoteChunk<'_>) -> ();
        fn mark_note_chunk_status(
            note_id: &str,
            chunk_idx: i64,
            content_hash: &str,
            note_revision: i64,
            status: &str,
            updated_at: i64,
        ) -> u64;
        fn delete_note_chunks_from(note_id: &str, min_chunk_idx: i64) -> u64;
        fn chunk_embedding_exists(note_id: &str, chunk_idx: i64) -> bool;
        fn enqueue_embedding_job(
            note_id: &str,
            chunk_idx: i64,
            content_hash: &str,
            content: &str,
            note_revision: i64,
            now: i64,
        ) -> ();
        fn delete_stale_embedding_jobs_for_chunk(
            note_id: &str,
            chunk_idx: i64,
            current_hash: &str,
        ) -> u64;
        fn delete_embedding_jobs_from_chunk(note_id: &str, min_chunk_idx: i64) -> u64;
        fn claim_pending_embedding_jobs(limit: usize, now: i64) -> Vec<EmbeddingJob>;
        fn delete_embedding_job(id: i64) -> u64;
        fn fail_embedding_job(
            id: i64,
            attempts: i64,
            max_attempts: i64,
            error: &str,
            now: i64,
        ) -> u64;
        fn requeue_processing_embedding_jobs(now: i64) -> u64;
        fn reset_embeddings_for_regeneration(now: i64) -> u64;
    }
}

impl_forward_repository! {
    RetrievalRepository {
        fn insert_chunk_embedding(note_id: &str, chunk_idx: i64, embedding: &[f32]) -> ();
        fn dense_search(
            query: &[f32],
            limit: usize,
            allowed_note_ids: Option<&[String]>,
        ) -> Vec<String>;
        fn title_search(
            query: &str,
            limit: usize,
            allowed_note_ids: Option<&[String]>,
        ) -> Vec<String>;
    }
}

impl_forward_repository! {
    SettingsRepository {
        fn get_system_config() -> note_core::SystemConfig;
        fn set_system_config(config: &note_core::SystemConfig) -> ();
        fn get_embedding_fingerprint() -> Option<String>;
        fn set_embedding_fingerprint(fingerprint: &str) -> ();
    }
}

impl_forward_repository! {
    OrgRepository {
        fn insert_org_workspace(value: NewOrgWorkspace<'_>) -> ();
        fn get_org_workspace(id: note_org::WorkspaceId) -> Option<OrgWorkspace>;
        fn get_org_workspace_by_slug(slug: &str) -> Option<OrgWorkspace>;
        fn list_org_workspaces(include_archived: bool) -> Vec<OrgWorkspace>;
        fn compare_and_swap_org_workspace(
            update: OrgWorkspaceUpdate<'_>,
        ) -> CompareAndSwap<OrgWorkspace>;
        fn insert_org_document(value: NewOrgDocument<'_>) -> ();
        fn get_org_document(id: note_org::DocumentId) -> Option<OrgDocument>;
        fn list_org_documents(
            workspace_id: note_org::WorkspaceId,
        ) -> Vec<OrgDocument>;
        fn compare_and_swap_org_document(
            update: OrgDocumentUpdate<'_>,
        ) -> CompareAndSwap<OrgDocument>;
        fn compare_and_swap_org_document_ownership(
            update: OrgDocumentOwnershipMove,
        ) -> OrgDocumentOwnershipMoveResult;
        fn replace_org_document_projection(
            document_id: note_org::DocumentId,
            items: &[OrgProjectedWorkItem],
        ) -> ();
        fn list_org_document_projection(
            document_id: note_org::DocumentId,
        ) -> Vec<OrgProjectedWorkItem>;
        fn get_org_work_item(
            id: note_org::WorkItemId,
        ) -> Option<OrgProjectedWorkItem>;
        fn list_org_workspace_projection(
            workspace_id: note_org::WorkspaceId,
        ) -> Vec<OrgProjectedWorkItem>;
        fn list_org_work_item_children(
            parent_id: note_org::WorkItemId,
        ) -> Vec<OrgProjectedWorkItem>;
        fn list_org_work_items_linking_note(
            note_id: &str,
        ) -> Vec<OrgProjectedWorkItem>;
        fn rebuild_org_workspace_projection(
            workspace_id: note_org::WorkspaceId,
            items: &[OrgProjectedWorkItem],
        ) -> ();
        fn append_org_event(event: NewOrgEvent<'_>) -> OrgEvent;
        fn append_internal_org_event(event: NewOrgEvent<'_>) -> OrgEvent;
        fn list_org_events(
            workspace_id: note_org::WorkspaceId,
            after_sequence: Option<i64>,
            limit: usize,
        ) -> Vec<OrgEvent>;
        fn list_org_subject_events(
            workspace_id: note_org::WorkspaceId,
            subject_kind: &str,
            subject_id: &str,
            after_sequence: Option<i64>,
            limit: usize,
        ) -> Vec<OrgEvent>;
        fn list_org_global_subject_events(
            subject_kind: &str,
            subject_id: &str,
        ) -> Vec<OrgEvent>;
        fn insert_org_attempt(attempt: NewOrgAttempt<'_>) -> OrgAttempt;
        fn get_org_attempt(id: &str) -> Option<OrgAttempt>;
        fn list_org_attempts(work_item_id: note_org::WorkItemId) -> Vec<OrgAttempt>;
        fn update_org_attempt(
            update: OrgAttemptUpdate<'_>,
        ) -> ConditionalUpdate<OrgAttempt>;
        fn insert_org_operation(operation: &StoredOrgOperation) -> ();
        fn get_org_operation(
            workspace_id: note_org::WorkspaceId,
            operation_id: &str,
        ) -> Option<StoredOrgOperation>;
    }
}

#[async_trait::async_trait]
impl StorageTransaction for EventTransaction {
    async fn commit(self: Box<Self>) -> StorageResult<()> {
        let Self {
            inner,
            events,
            fail_commit,
        } = *self;
        if fail_commit.swap(false, Ordering::SeqCst) {
            inner.rollback().await?;
            events.lock().unwrap().push("commit_failed".into());
            return Err(StorageError::new(
                StorageErrorKind::Transaction,
                "controlled commit failure",
            ));
        }
        inner.commit().await?;
        events.lock().unwrap().push("commit".into());
        Ok(())
    }

    async fn rollback(self: Box<Self>) -> StorageResult<()> {
        let Self {
            inner,
            events,
            fail_commit: _,
        } = *self;
        inner.rollback().await?;
        events.lock().unwrap().push("rollback".into());
        Ok(())
    }
}

pub struct ControlledAttachmentStore {
    backend: Arc<dyn StorageBackend>,
    events: EventLog,
    fail_publish: AtomicBool,
    fail_abort: AtomicBool,
    race_note_ids: Mutex<HashSet<String>>,
    delete_path_races: Mutex<HashMap<String, VecDeque<(String, String)>>>,
    read_contents: Mutex<HashMap<(String, String), Vec<u8>>>,
    read_failures: Mutex<HashMap<(String, String), VecDeque<String>>>,
    read_path_races: Mutex<HashMap<(String, String), VecDeque<Vec<(String, String)>>>>,
    committed_marker: Mutex<Option<String>>,
    blocked_publish_note: Mutex<Option<String>>,
    publish_started: Arc<Notify>,
    publish_release: Arc<Notify>,
}

impl ControlledAttachmentStore {
    pub fn new(backend: Arc<dyn StorageBackend>, events: EventLog) -> Self {
        Self {
            backend,
            events,
            fail_publish: AtomicBool::new(false),
            fail_abort: AtomicBool::new(false),
            race_note_ids: Mutex::new(HashSet::new()),
            delete_path_races: Mutex::new(HashMap::new()),
            read_contents: Mutex::new(HashMap::new()),
            read_failures: Mutex::new(HashMap::new()),
            read_path_races: Mutex::new(HashMap::new()),
            committed_marker: Mutex::new(None),
            blocked_publish_note: Mutex::new(None),
            publish_started: Arc::new(Notify::new()),
            publish_release: Arc::new(Notify::new()),
        }
    }

    pub fn fail_publish(&self) {
        self.fail_publish.store(true, Ordering::SeqCst);
    }

    pub fn fail_abort(&self) {
        self.fail_abort.store(true, Ordering::SeqCst);
    }

    pub fn race_note_on_prepare(&self, note_id: &str) {
        self.race_note_ids
            .lock()
            .unwrap()
            .insert(note_id.to_string());
    }

    pub fn race_attachment_path_on_delete(&self, note_id: &str, attachment_id: &str, path: &str) {
        self.delete_path_races
            .lock()
            .unwrap()
            .entry(note_id.to_string())
            .or_default()
            .push_back((attachment_id.to_string(), path.to_string()));
    }

    pub fn set_read_content(&self, note_id: &str, path: &str, content: &[u8]) {
        self.read_contents
            .lock()
            .unwrap()
            .insert((note_id.to_string(), path.to_string()), content.to_vec());
    }

    pub fn fail_read_once(&self, note_id: &str, path: &str, message: &str) {
        self.read_failures
            .lock()
            .unwrap()
            .entry((note_id.to_string(), path.to_string()))
            .or_default()
            .push_back(message.to_string());
    }

    pub fn race_attachment_paths_after_read(
        &self,
        note_id: &str,
        path: &str,
        replacements: &[(&str, &str)],
    ) {
        self.read_path_races
            .lock()
            .unwrap()
            .entry((note_id.to_string(), path.to_string()))
            .or_default()
            .push_back(
                replacements
                    .iter()
                    .map(|(attachment_id, path)| (attachment_id.to_string(), path.to_string()))
                    .collect(),
            );
    }

    pub fn observe_committed_label_on_abort(&self, key: &str) {
        *self.committed_marker.lock().unwrap() = Some(key.to_string());
    }

    pub fn block_publish(&self, note_id: &str) {
        *self.blocked_publish_note.lock().unwrap() = Some(note_id.to_string());
    }

    pub async fn wait_for_blocked_publish(&self) {
        self.publish_started.notified().await;
    }

    pub fn release_blocked_publish(&self) {
        self.publish_release.notify_one();
    }
}

struct ControlledPreparedSet {
    note_id: String,
    metadata: Vec<NoteAttachment>,
    backend: Arc<dyn StorageBackend>,
    events: EventLog,
    fail_publish: bool,
    fail_abort: bool,
    committed_marker: Option<String>,
    block_publish: bool,
    publish_started: Arc<Notify>,
    publish_release: Arc<Notify>,
}

struct ControlledPreparedMutation {
    note_id: String,
    path: String,
    operation: &'static str,
    events: EventLog,
    fail_publish: bool,
    fail_abort: bool,
}

#[async_trait::async_trait]
impl AttachmentStore for ControlledAttachmentStore {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>> {
        let session = self.backend.session().await?;
        let state = session
            .list_all_notes()
            .await?
            .into_iter()
            .find(|note| note.id == note_id)
            .map_or_else(|| "missing".to_string(), |note| note.title);
        self.events
            .lock()
            .unwrap()
            .push(format!("prepare:{note_id}:{state}"));

        if self.race_note_ids.lock().unwrap().remove(note_id) {
            session
                .insert_note(NewNote {
                    id: note_id,
                    title: "Raced",
                    content: "Inserted during attachment preparation",
                    attachments: &[],
                    created_at: 1,
                    updated_at: 1,
                    note_revision: 1,
                    deleted_at: None,
                })
                .await?;
        }

        Ok(Box::new(ControlledPreparedSet {
            note_id: note_id.to_string(),
            metadata: attachments
                .iter()
                .map(|attachment| NoteAttachment {
                    id: attachment.id.clone(),
                    path: attachment.path.clone(),
                    mime: attachment.mime.clone(),
                    description: attachment.description.clone(),
                    content: Vec::new(),
                })
                .collect(),
            backend: self.backend.clone(),
            events: self.events.clone(),
            fail_publish: self.fail_publish.load(Ordering::SeqCst),
            fail_abort: self.fail_abort.load(Ordering::SeqCst),
            committed_marker: self.committed_marker.lock().unwrap().clone(),
            block_publish: self.blocked_publish_note.lock().unwrap().as_deref() == Some(note_id),
            publish_started: self.publish_started.clone(),
            publish_release: self.publish_release.clone(),
        }))
    }

    async fn prepare_put(
        &self,
        note_id: &str,
        attachment: &NoteAttachment,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        self.events
            .lock()
            .unwrap()
            .push(format!("prepare_put:{note_id}:{}", attachment.path));
        Ok(Box::new(ControlledPreparedMutation {
            note_id: note_id.to_string(),
            path: attachment.path.clone(),
            operation: "put",
            events: self.events.clone(),
            fail_publish: self.fail_publish.load(Ordering::SeqCst),
            fail_abort: self.fail_abort.load(Ordering::SeqCst),
        }))
    }

    async fn prepare_delete(
        &self,
        note_id: &str,
        path: &str,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        self.events
            .lock()
            .unwrap()
            .push(format!("prepare_delete:{note_id}:{path}"));
        let race = self
            .delete_path_races
            .lock()
            .unwrap()
            .get_mut(note_id)
            .and_then(VecDeque::pop_front);
        if let Some((attachment_id, replacement_path)) = race {
            let session = self.backend.session().await?;
            if let Some(mut note) = session.get_note(note_id).await? {
                if let Some(attachment) = note
                    .attachments
                    .iter_mut()
                    .find(|attachment| attachment.id == attachment_id)
                {
                    attachment.path = replacement_path.clone();
                    session
                        .update_note_attachments(AttachmentMetadataUpdate {
                            id: note_id,
                            attachments: &note.attachments,
                            updated_at: note.updated_at.saturating_add(1),
                        })
                        .await?;
                    self.events.lock().unwrap().push(format!(
                        "race_delete_path:{note_id}:{attachment_id}:{replacement_path}"
                    ));
                }
            }
        }
        Ok(Box::new(ControlledPreparedMutation {
            note_id: note_id.to_string(),
            path: path.to_string(),
            operation: "delete",
            events: self.events.clone(),
            fail_publish: self.fail_publish.load(Ordering::SeqCst),
            fail_abort: self.fail_abort.load(Ordering::SeqCst),
        }))
    }

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        self.events
            .lock()
            .unwrap()
            .push(format!("read:{note_id}:{path}"));
        let key = (note_id.to_string(), path.to_string());
        let content = self
            .read_contents
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let failure = self
            .read_failures
            .lock()
            .unwrap()
            .get_mut(&key)
            .and_then(VecDeque::pop_front);
        let race = self
            .read_path_races
            .lock()
            .unwrap()
            .get_mut(&key)
            .and_then(VecDeque::pop_front);
        if let Some(replacements) = race {
            let session = self.backend.session().await?;
            if let Some(mut note) = session.get_note(note_id).await? {
                for (attachment_id, replacement_path) in replacements {
                    if let Some(attachment) = note
                        .attachments
                        .iter_mut()
                        .find(|attachment| attachment.id == attachment_id)
                    {
                        attachment.path = replacement_path;
                    }
                }
                session
                    .update_note_attachments(AttachmentMetadataUpdate {
                        id: note_id,
                        attachments: &note.attachments,
                        updated_at: note.updated_at.saturating_add(1),
                    })
                    .await?;
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("race_read_paths:{note_id}:{path}"));
            }
        }
        if let Some(message) = failure {
            anyhow::bail!(message);
        }
        Ok(content)
    }

    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
        let session = self.backend.session().await?;
        let absent = session
            .list_all_notes()
            .await?
            .iter()
            .all(|note| note.id != note_id);
        self.events
            .lock()
            .unwrap()
            .push(format!("remove:{note_id}:absent={absent}"));
        Ok(())
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "controlled".into(),
            location: None,
        }
    }
}

#[async_trait::async_trait]
impl PreparedAttachmentMutation for ControlledPreparedMutation {
    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(format!(
            "publish_{}:{}:{}",
            self.operation, self.note_id, self.path
        ));
        if self.fail_publish {
            anyhow::bail!("controlled mutation publish failure");
        }
        Ok(())
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(format!(
            "abort_{}:{}:{}",
            self.operation, self.note_id, self.path
        ));
        if self.fail_abort {
            anyhow::bail!("controlled mutation abort failure");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl PreparedAttachmentSet for ControlledPreparedSet {
    fn metadata(&self) -> &[NoteAttachment] {
        &self.metadata
    }

    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        if self.block_publish {
            self.publish_started.notify_one();
            self.publish_release.notified().await;
        }
        let session = self.backend.session().await?;
        let state = session
            .list_all_notes()
            .await?
            .into_iter()
            .find(|note| note.id == self.note_id)
            .map_or_else(|| "missing".to_string(), |note| note.title);
        self.events
            .lock()
            .unwrap()
            .push(format!("publish:{}:{state}", self.note_id));
        if self.fail_publish {
            anyhow::bail!("controlled publish failure");
        }
        Ok(())
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        let marker = if let Some(key) = &self.committed_marker {
            let session = self.backend.session().await?;
            session
                .list_label_keys()
                .await?
                .iter()
                .any(|label| &label.key == key)
        } else {
            false
        };
        self.events
            .lock()
            .unwrap()
            .push(format!("abort:{}:marker={marker}", self.note_id));
        if self.fail_abort {
            anyhow::bail!("controlled abort failure");
        }
        Ok(())
    }
}

pub struct EventNotifier {
    events: EventLog,
}

impl EventNotifier {
    pub fn new(events: EventLog) -> Self {
        Self { events }
    }
}

impl EmbeddingJobNotifier for EventNotifier {
    fn wake(&self) {
        self.events.lock().unwrap().push("wake".into());
    }
}
