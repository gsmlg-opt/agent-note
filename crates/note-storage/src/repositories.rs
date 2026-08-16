use crate::{
    ActiveNoteSource, AttachmentMetadataUpdate, BackendInfo, CompareAndSwap,
    EmbeddingDashboardStatus, EmbeddingJob, NewNote, NewOrgAttempt, NewOrgDocument, NewOrgEvent,
    NewOrgLease, NewOrgWorkspace, NoteChunk, NoteFieldsUpdate, NoteUpdate, OrgArtifactReference,
    OrgAttempt, OrgAttemptNoteReference, OrgAttemptUpdate, OrgDocument, OrgDocumentUpdate,
    OrgEvent, OrgLease, OrgLeaseEndReason, OrgLeaseKind, OrgOperationalQuery, OrgOperationalRow,
    OrgProjectedWorkItem, OrgWorkspace, OrgWorkspaceOperationalSummary, OrgWorkspaceUpdate,
    StorageError, StorageErrorKind, StorageResult, StoredOrgOperation, UpsertNoteChunk,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Orders global subject events as a single lineage of workspace-local
/// sequence segments. Every cross-workspace segment must link its first event
/// to the exact last event of its predecessor segment.
pub fn order_org_event_segments_by_lineage(events: Vec<OrgEvent>) -> StorageResult<Vec<OrgEvent>> {
    if events.is_empty() {
        return Ok(Vec::new());
    }

    let corrupt = |message| {
        StorageError::new(
            StorageErrorKind::Corrupt,
            format!("invalid Org event lineage: {message}"),
        )
    };
    let mut event_workspaces = HashMap::new();
    for event in &events {
        if event_workspaces
            .insert(event.id.clone(), event.workspace_id)
            .is_some()
        {
            return Err(corrupt("duplicate event id"));
        }
    }

    let mut workspace_events = BTreeMap::<note_org::WorkspaceId, Vec<OrgEvent>>::new();
    for event in events {
        workspace_events
            .entry(event.workspace_id)
            .or_default()
            .push(event);
    }
    for events in workspace_events.values_mut() {
        events.sort_by_key(|event| event.sequence);
    }

    let mut segments = Vec::<Vec<OrgEvent>>::new();
    for (workspace_id, events) in workspace_events {
        let mut segment = Vec::new();
        for event in events {
            let previous_id = lineage_previous_event_id(&event)?;
            let starts_segment = match previous_id {
                Some(previous_id) => {
                    let previous_workspace = event_workspaces
                        .get(previous_id)
                        .ok_or_else(|| corrupt("predecessor event does not exist"))?;
                    if *previous_workspace == workspace_id {
                        return Err(corrupt(
                            "lineage predecessor must be in a different workspace",
                        ));
                    }
                    true
                }
                None => false,
            };
            if starts_segment && !segment.is_empty() {
                segments.push(std::mem::take(&mut segment));
            }
            segment.push(event);
        }
        if !segment.is_empty() {
            segments.push(segment);
        }
    }

    let mut event_segments = HashMap::new();
    for (segment_index, segment) in segments.iter().enumerate() {
        for event in segment {
            event_segments.insert(event.id.as_str(), segment_index);
        }
    }

    let mut successor = vec![None; segments.len()];
    let mut roots = Vec::new();
    for (segment_index, segment) in segments.iter().enumerate() {
        let first = segment
            .first()
            .ok_or_else(|| corrupt("empty event segment"))?;
        let Some(previous_id) = lineage_previous_event_id(first)? else {
            roots.push(segment_index);
            continue;
        };
        let previous_segment = *event_segments
            .get(previous_id)
            .ok_or_else(|| corrupt("predecessor event does not exist"))?;
        let previous_last = segments[previous_segment]
            .last()
            .ok_or_else(|| corrupt("empty predecessor segment"))?;
        if previous_last.id != previous_id {
            return Err(corrupt(
                "predecessor link does not reference the source segment boundary",
            ));
        }
        if successor[previous_segment].replace(segment_index).is_some() {
            return Err(corrupt("one segment has multiple successors"));
        }
    }

    if roots.len() != 1 {
        return Err(corrupt(
            "lineage must contain exactly one unambiguous root segment",
        ));
    }
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut current = Some(roots[0]);
    while let Some(segment_index) = current {
        if !visited.insert(segment_index) {
            return Err(corrupt("lineage contains a cycle"));
        }
        ordered.extend(segments[segment_index].iter().cloned());
        current = successor[segment_index];
    }
    if visited.len() != segments.len() {
        return Err(corrupt("lineage is disconnected or cyclic"));
    }
    Ok(ordered)
}

fn lineage_previous_event_id(event: &OrgEvent) -> StorageResult<Option<&str>> {
    match event.metadata.get("lineage_previous_event_id") {
        None => Ok(None),
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => Ok(Some(value)),
        Some(_) => Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "invalid Org event lineage: predecessor id must be a non-empty string",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalUpdate<T> {
    Applied(T),
    NotFound,
    Conflict,
}

/// Lease metadata safe for general reads. Fencing-token hashes are deliberately
/// absent so neither a raw token nor its verifier can escape through read APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedOrgLease {
    pub id: String,
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
    pub attempt_id: String,
    pub kind: OrgLeaseKind,
    pub actor_id: String,
    pub acquired_at: i64,
    pub last_heartbeat_at: i64,
    pub expires_at: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<OrgLeaseEndReason>,
    pub expiry_event_id: Option<String>,
}

impl From<OrgLease> for SanitizedOrgLease {
    fn from(lease: OrgLease) -> Self {
        Self {
            id: lease.id,
            workspace_id: lease.workspace_id,
            work_item_id: lease.work_item_id,
            attempt_id: lease.attempt_id,
            kind: lease.kind,
            actor_id: lease.actor_id,
            acquired_at: lease.acquired_at,
            last_heartbeat_at: lease.last_heartbeat_at,
            expires_at: lease.expires_at,
            ended_at: lease.ended_at,
            end_reason: lease.end_reason,
            expiry_event_id: lease.expiry_event_id,
        }
    }
}

pub struct NewOrgAttemptAllocation<'a> {
    pub id: &'a str,
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
    pub actor_id: &'a str,
    pub started_at: i64,
    pub note_refs: &'a [OrgAttemptNoteReference],
    pub artifacts: &'a [OrgArtifactReference],
    pub metadata: &'a serde_json::Value,
}

#[derive(Clone, Copy)]
pub struct OrgLeaseProof<'a> {
    pub lease_id: &'a str,
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
    pub fencing_token_hash: &'a str,
    pub kind: OrgLeaseKind,
    pub actor_id: &'a str,
    pub now: i64,
}

pub struct OrgLeaseHeartbeat<'a> {
    pub proof: OrgLeaseProof<'a>,
    pub last_heartbeat_at: i64,
    pub expires_at: i64,
}

pub struct OrgLeaseClosure<'a> {
    pub proof: OrgLeaseProof<'a>,
    pub ended_at: i64,
    pub end_reason: OrgLeaseEndReason,
}

pub struct ExpiredOrgLeaseClosure<'a> {
    pub lease_id: &'a str,
    pub work_item_id: note_org::WorkItemId,
    pub now: i64,
    pub ended_at: i64,
    pub expiry_event_id: &'a str,
}

pub struct OrgLeaseOwnershipMove<'a> {
    pub proof: OrgLeaseProof<'a>,
    pub target_workspace_id: note_org::WorkspaceId,
    pub target_capacity: i64,
}

pub struct OrgDocumentOwnershipMove {
    pub document_id: note_org::DocumentId,
    pub source_workspace_id: note_org::WorkspaceId,
    pub target_workspace_id: note_org::WorkspaceId,
    pub expected_document_revision: i64,
    pub expected_source_workspace_revision: i64,
    pub expected_target_workspace_revision: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedOrgDocumentOwnershipMove {
    pub document: OrgDocument,
    pub source_workspace_revision: i64,
    pub target_workspace_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgDocumentOwnershipMoveResult {
    Applied(AppliedOrgDocumentOwnershipMove),
    NotFound,
    Conflict {
        current_document_workspace_id: note_org::WorkspaceId,
        current_document_revision: i64,
        current_source_workspace_revision: i64,
        current_target_workspace_revision: i64,
    },
}

#[async_trait::async_trait]
pub trait NotesRepository: Send + Sync {
    async fn insert_note(&self, note: NewNote<'_>) -> StorageResult<()>;
    async fn get_note_revision(&self, id: &str) -> StorageResult<Option<i64>>;
    async fn note_exists(&self, id: &str) -> StorageResult<bool>;
    async fn get_note(&self, id: &str) -> StorageResult<Option<note_core::Note>>;
    async fn get_note_content(&self, id: &str) -> StorageResult<Option<String>>;
    async fn update_note(&self, note: NoteUpdate<'_>) -> StorageResult<u64>;
    async fn update_note_fields(&self, note: NoteFieldsUpdate<'_>) -> StorageResult<u64>;
    async fn update_note_attachments(
        &self,
        note: AttachmentMetadataUpdate<'_>,
    ) -> StorageResult<u64>;
    async fn soft_delete_note(&self, id: &str, deleted_at: i64) -> StorageResult<u64>;
    async fn get_deleted_note_content_and_revision(
        &self,
        id: &str,
    ) -> StorageResult<Option<(String, i64)>>;
    async fn restore_note(&self, id: &str, note_revision: i64) -> StorageResult<u64>;
    async fn permanently_delete_note(&self, id: &str) -> StorageResult<u64>;
    async fn list_expired_deleted_note_ids(&self, cutoff: i64) -> StorageResult<Vec<String>>;
    async fn clear_note_search_data(&self, id: &str) -> StorageResult<()>;
    async fn clear_note_labels(&self, id: &str) -> StorageResult<()>;
    async fn clear_note_chunk_derived(&self, id: &str, chunk_idx: i64) -> StorageResult<()>;
    async fn clear_note_chunks_from_derived(
        &self,
        id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<()>;
    async fn list_notes(
        &self,
        selectors: &[note_core::LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<note_core::Note>>;
    async fn list_all_notes(&self) -> StorageResult<Vec<note_core::Note>>;
    async fn list_note_summaries(
        &self,
        selectors: &[note_core::LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<note_core::NoteListItem>>;
    async fn list_deleted_note_summaries(&self) -> StorageResult<Vec<note_core::NoteListItem>>;
    async fn count_notes(&self, selectors: &[note_core::LabelSelector]) -> StorageResult<usize>;
    async fn matching_note_ids(
        &self,
        selectors: &[note_core::LabelSelector],
    ) -> StorageResult<Vec<String>>;
    /// Returns sorted IDs for matching active notes while locking those note
    /// rows until the enclosing transaction ends. Call through an explicit
    /// storage transaction to retain the locks across subsequent writes.
    async fn matching_note_ids_for_update(
        &self,
        selectors: &[note_core::LabelSelector],
    ) -> StorageResult<Vec<String>>;
    /// Advances an active note's label-only mutation timestamp monotonically
    /// without changing its content or note revision.
    async fn advance_note_updated_at(&self, id: &str, now: i64) -> StorageResult<u64>;
    async fn list_active_note_sources(&self) -> StorageResult<Vec<ActiveNoteSource>>;
}

#[async_trait::async_trait]
pub trait LabelRepository: Send + Sync {
    async fn insert_label_key(&self, key: &str, description: &str) -> StorageResult<()>;
    async fn insert_label_key_if_missing(&self, key: &str, description: &str) -> StorageResult<()>;
    async fn insert_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: note_core::LabelValueType,
    ) -> StorageResult<()>;
    async fn list_label_keys(&self) -> StorageResult<Vec<note_core::LabelKey>>;
    async fn update_label_key(&self, key: &str, description: &str) -> StorageResult<()>;
    async fn update_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: note_core::LabelValueType,
    ) -> StorageResult<()>;
    async fn delete_label_key(&self, key: &str) -> StorageResult<()>;
    async fn attach_label(&self, note_id: &str, key: &str, value: &str) -> StorageResult<()>;
    /// Inserts or replaces one note label, returning whether its desired value
    /// changed.
    async fn set_note_label(&self, note_id: &str, key: &str, value: &str) -> StorageResult<bool>;
    async fn labels_for_note(&self, note_id: &str) -> StorageResult<Vec<note_core::Label>>;
    async fn label_note_counts(&self) -> StorageResult<Vec<(String, usize)>>;
    async fn find_note_with_labels(
        &self,
        labels: &[(String, String)],
    ) -> StorageResult<Option<String>>;
}

#[async_trait::async_trait]
pub trait EmbeddingRepository: Send + Sync {
    async fn embedding_dashboard_status(&self) -> StorageResult<EmbeddingDashboardStatus>;
    async fn list_note_chunks(&self, note_id: &str) -> StorageResult<Vec<NoteChunk>>;
    async fn get_note_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
    ) -> StorageResult<Option<NoteChunk>>;
    async fn upsert_note_chunk(&self, chunk: UpsertNoteChunk<'_>) -> StorageResult<()>;
    async fn mark_note_chunk_status(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        note_revision: i64,
        status: &str,
        updated_at: i64,
    ) -> StorageResult<u64>;
    async fn delete_note_chunks_from(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64>;
    async fn chunk_embedding_exists(&self, note_id: &str, chunk_idx: i64) -> StorageResult<bool>;
    async fn enqueue_embedding_job(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        content: &str,
        note_revision: i64,
        now: i64,
    ) -> StorageResult<()>;
    async fn delete_stale_embedding_jobs_for_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
        current_hash: &str,
    ) -> StorageResult<u64>;
    async fn delete_embedding_jobs_from_chunk(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64>;
    async fn claim_pending_embedding_jobs(
        &self,
        limit: usize,
        now: i64,
    ) -> StorageResult<Vec<EmbeddingJob>>;
    async fn delete_embedding_job(&self, id: i64) -> StorageResult<u64>;
    async fn fail_embedding_job(
        &self,
        id: i64,
        attempts: i64,
        max_attempts: i64,
        error: &str,
        now: i64,
    ) -> StorageResult<u64>;
    async fn requeue_processing_embedding_jobs(&self, now: i64) -> StorageResult<u64>;
    /// Removes every derived vector, marks active chunks pending, replaces all
    /// embedding jobs with one pending job per active chunk, and returns the
    /// number of queued jobs. Call this inside a storage transaction together
    /// with `SettingsRepository::set_embedding_fingerprint`.
    async fn reset_embeddings_for_regeneration(&self, now: i64) -> StorageResult<u64>;
}

#[async_trait::async_trait]
pub trait RetrievalRepository: Send + Sync {
    async fn insert_chunk_embedding(
        &self,
        note_id: &str,
        chunk_idx: i64,
        embedding: &[f32],
    ) -> StorageResult<()>;
    async fn dense_search(
        &self,
        query: &[f32],
        limit: usize,
        allowed_note_ids: Option<&[String]>,
    ) -> StorageResult<Vec<String>>;
    async fn title_search(
        &self,
        query: &str,
        limit: usize,
        allowed_note_ids: Option<&[String]>,
    ) -> StorageResult<Vec<String>>;
}

#[async_trait::async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn get_system_config(&self) -> StorageResult<note_core::SystemConfig>;
    async fn set_system_config(&self, config: &note_core::SystemConfig) -> StorageResult<()>;
    async fn get_embedding_fingerprint(&self) -> StorageResult<Option<String>>;
    async fn set_embedding_fingerprint(&self, fingerprint: &str) -> StorageResult<()>;
}

#[async_trait::async_trait]
pub trait OrgRepository: Send + Sync {
    async fn insert_org_workspace(&self, value: NewOrgWorkspace<'_>) -> StorageResult<()> {
        let _ = value;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_org_workspace(
        &self,
        id: note_org::WorkspaceId,
    ) -> StorageResult<Option<OrgWorkspace>> {
        let _ = id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_org_workspace_by_slug(&self, slug: &str) -> StorageResult<Option<OrgWorkspace>> {
        let _ = slug;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_workspaces(
        &self,
        include_archived: bool,
    ) -> StorageResult<Vec<OrgWorkspace>> {
        let _ = include_archived;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn compare_and_swap_org_workspace(
        &self,
        update: OrgWorkspaceUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgWorkspace>> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn insert_org_document(&self, value: NewOrgDocument<'_>) -> StorageResult<()> {
        let _ = value;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_org_document(
        &self,
        id: note_org::DocumentId,
    ) -> StorageResult<Option<OrgDocument>> {
        let _ = id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_documents(
        &self,
        workspace_id: note_org::WorkspaceId,
    ) -> StorageResult<Vec<OrgDocument>> {
        let _ = workspace_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn compare_and_swap_org_document(
        &self,
        update: OrgDocumentUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgDocument>> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Atomically moves one canonical document between distinct workspaces.
    /// The document and both workspace revisions must all match; conflicts do
    /// not change any of the three records.
    async fn compare_and_swap_org_document_ownership(
        &self,
        update: OrgDocumentOwnershipMove,
    ) -> StorageResult<OrgDocumentOwnershipMoveResult> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Must be called inside a storage transaction with the document write.
    async fn replace_org_document_projection(
        &self,
        document_id: note_org::DocumentId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let _ = (document_id, items);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_document_projection(
        &self,
        document_id: note_org::DocumentId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _ = document_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_org_work_item(
        &self,
        id: note_org::WorkItemId,
    ) -> StorageResult<Option<OrgProjectedWorkItem>> {
        let _ = id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_workspace_projection(
        &self,
        workspace_id: note_org::WorkspaceId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _ = workspace_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_work_item_children(
        &self,
        parent_id: note_org::WorkItemId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _ = parent_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_work_items_linking_note(
        &self,
        note_id: &str,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _ = note_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Rebuilds every projection row for a workspace from all canonical documents; events and operation records are not modified.
    async fn rebuild_org_workspace_projection(
        &self,
        workspace_id: note_org::WorkspaceId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let _ = (workspace_id, items);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Must be called inside the same transaction as the mutation it records.
    async fn append_org_event(&self, event: NewOrgEvent<'_>) -> StorageResult<OrgEvent> {
        let _ = event;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Internal audit append for events whose actor is the reserved `system`
    /// principal. Client-originated code must use `append_org_event`.
    async fn append_internal_org_event(&self, event: NewOrgEvent<'_>) -> StorageResult<OrgEvent> {
        let _ = event;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_events(
        &self,
        workspace_id: note_org::WorkspaceId,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>> {
        let _ = (workspace_id, after_sequence, limit);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_subject_events(
        &self,
        workspace_id: note_org::WorkspaceId,
        subject_kind: &str,
        subject_id: &str,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>> {
        let _ = (
            workspace_id,
            subject_kind,
            subject_id,
            after_sequence,
            limit,
        );
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Returns global subject history grouped deterministically by workspace,
    /// preserving each workspace's authoritative sequence without inventing a
    /// cross-workspace counter.
    async fn list_org_global_subject_events(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> StorageResult<Vec<OrgEvent>> {
        let _ = (subject_kind, subject_id);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn insert_org_attempt(&self, attempt: NewOrgAttempt<'_>) -> StorageResult<OrgAttempt> {
        let _ = attempt;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Allocates the next stable work-item attempt number. Callers must invoke
    /// this inside the immediate transaction that owns the surrounding claim.
    async fn allocate_next_org_attempt(
        &self,
        attempt: NewOrgAttemptAllocation<'_>,
    ) -> StorageResult<OrgAttempt> {
        let _ = attempt;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_org_attempt(&self, id: &str) -> StorageResult<Option<OrgAttempt>> {
        let _ = id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_attempts(
        &self,
        work_item_id: note_org::WorkItemId,
    ) -> StorageResult<Vec<OrgAttempt>> {
        let _ = work_item_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn count_org_attempts(&self, work_item_id: note_org::WorkItemId) -> StorageResult<i64> {
        let _ = work_item_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn update_org_attempt(
        &self,
        update: OrgAttemptUpdate<'_>,
    ) -> StorageResult<ConditionalUpdate<OrgAttempt>> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Atomically enforces both one-open-per-item and workspace capacity.
    /// The surrounding claim transaction must use `TransactionMode::Immediate`.
    async fn insert_org_lease_if_capacity(
        &self,
        lease: NewOrgLease<'_>,
        capacity: i64,
        now: i64,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _ = (lease, capacity, now);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Internal fencing lookup. General read paths must use sanitized methods.
    async fn get_open_org_lease_internal(
        &self,
        work_item_id: note_org::WorkItemId,
    ) -> StorageResult<Option<OrgLease>> {
        let _ = work_item_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_active_org_lease(
        &self,
        work_item_id: note_org::WorkItemId,
        now: i64,
    ) -> StorageResult<Option<SanitizedOrgLease>> {
        let _ = (work_item_id, now);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn list_org_lease_history(
        &self,
        work_item_id: note_org::WorkItemId,
    ) -> StorageResult<Vec<SanitizedOrgLease>> {
        let _ = work_item_id;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn count_active_org_leases(
        &self,
        workspace_id: note_org::WorkspaceId,
        now: i64,
    ) -> StorageResult<i64> {
        let _ = (workspace_id, now);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Validates an active lease fence without extending, closing, or otherwise
    /// changing the lease. A missing or stale proof is reported as a conflict.
    async fn validate_org_lease_proof(
        &self,
        proof: OrgLeaseProof<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _ = proof;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn heartbeat_org_lease(
        &self,
        update: OrgLeaseHeartbeat<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn close_org_lease(
        &self,
        update: OrgLeaseClosure<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn close_expired_org_lease(
        &self,
        update: ExpiredOrgLeaseClosure<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Moves current lease ownership without rotating or closing its fence.
    /// The target-capacity check shares the immediate transaction serialization
    /// domain with lease insertion.
    async fn move_org_lease_ownership(
        &self,
        update: OrgLeaseOwnershipMove<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _ = update;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn query_org_operational(
        &self,
        query: OrgOperationalQuery<'_>,
    ) -> StorageResult<Vec<OrgOperationalRow>> {
        let _ = query;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_org_workspace_operational_summary(
        &self,
        workspace_id: note_org::WorkspaceId,
        now: i64,
    ) -> StorageResult<Option<OrgWorkspaceOperationalSummary>> {
        let _ = (workspace_id, now);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    /// Must be called inside the same transaction as the mutation result.
    async fn insert_org_operation(&self, operation: &StoredOrgOperation) -> StorageResult<()> {
        let _ = operation;
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }

    async fn get_org_operation(
        &self,
        workspace_id: note_org::WorkspaceId,
        operation_id: &str,
    ) -> StorageResult<Option<StoredOrgOperation>> {
        let _ = (workspace_id, operation_id);
        Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            "Org persistence is not implemented by this storage session",
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionMode {
    Deferred,
    Immediate,
}

pub trait StorageSession:
    NotesRepository
    + LabelRepository
    + EmbeddingRepository
    + RetrievalRepository
    + SettingsRepository
    + OrgRepository
    + Send
    + Sync
{
}

impl<T> StorageSession for T where
    T: NotesRepository
        + LabelRepository
        + EmbeddingRepository
        + RetrievalRepository
        + SettingsRepository
        + OrgRepository
        + Send
        + Sync
{
}

#[async_trait::async_trait]
pub trait StorageTransaction: StorageSession {
    async fn commit(self: Box<Self>) -> StorageResult<()>;
    async fn rollback(self: Box<Self>) -> StorageResult<()>;
}

#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    async fn session(&self) -> StorageResult<Box<dyn StorageSession>>;
    async fn begin(&self, mode: TransactionMode) -> StorageResult<Box<dyn StorageTransaction>>;
    async fn info(&self) -> StorageResult<BackendInfo>;
}
