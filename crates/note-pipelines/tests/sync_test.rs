mod support;
use note_core::{sync::*, NoteAttachment, SystemConfig};
use note_pipelines::{sync::*, *};
use note_storage::{NewNote, StorageBackend, TransactionMode};
use std::{collections::HashMap, sync::Arc};

fn config() -> SystemConfig {
    SystemConfig {
        sync: SyncConfig {
            enabled: true,
            connection_profile: Some("gao_note".into()),
            rules: vec![
                SyncRule {
                    id: "project".into(),
                    enabled: true,
                    selector: "project=bear_note".into(),
                },
                SyncRule {
                    id: "team".into(),
                    enabled: true,
                    selector: "team=notes".into(),
                },
            ],
        },
        ..Default::default()
    }
}
fn input(content: &str) -> SaveNoteInput {
    SaveNoteInput {
        title: "Synchronized".into(),
        content: content.into(),
        labels: vec![("project".into(), "bear_note".into())],
        attachments: vec![],
    }
}
async fn enabled(ctx: &Context) {
    update_system_config(ctx, &config()).await.unwrap();
    initialize_sync(ctx).await.unwrap();
}
fn remote(mut event: SyncEvent, kind: SyncEventKind, content: &str) -> SyncEvent {
    event.event_id = uuid::Uuid::new_v4().to_string();
    event.kind = kind;
    event.version.counter += 1;
    event.version.node_id = "11111111-1111-4111-8111-111111111111".into();
    if let Some(snapshot) = &mut event.snapshot {
        snapshot.content = content.into();
    }
    event.snapshot_hash = canonical_snapshot_hash(event.snapshot.as_ref()).unwrap();
    event
}

#[tokio::test]
async fn state_probe_is_read_only_and_missing_association_recovery_is_conditional() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("probe source")).await.unwrap();
    let original = next_sync_event(&ctx).await.unwrap().unwrap();
    let query = SyncStateQuery {
        query_id: uuid::Uuid::new_v4().to_string(),
        items: vec![original.acknowledgement()],
    };
    let response = query_sync_state(&ctx, &query).await.unwrap();
    assert!(response.matches_query(&query));
    assert_eq!(response.items[0].association, SyncAssociation::Attached);
    assert_eq!(
        response.items[0]
            .head
            .as_ref()
            .unwrap()
            .attachments_available,
        Some(true)
    );
    assert_eq!(
        sync_status(&ctx).await.unwrap().pending_notes,
        1,
        "probe cannot ACK pending work"
    );
    acknowledge_sync_event(&ctx, &original.acknowledgement())
        .await
        .unwrap();
    let page = sync_candidates(&ctx, None, 32).await.unwrap();
    assert_eq!(page.len(), 1);
    assert!(!page[0].pending);
    let normalized = page[0].descriptor.clone();
    assert_eq!(normalized.kind, SyncEventKind::Snapshot);
    assert_ne!(normalized.event_id, original.event_id);
    assert!(sync_candidate_current(&ctx, &normalized, true)
        .await
        .unwrap());
    assert!(recover_missing_sync_association(&ctx, &normalized)
        .await
        .unwrap());
    assert!(!recover_missing_sync_association(&ctx, &normalized)
        .await
        .unwrap());
    let recovery = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(recovery.kind, SyncEventKind::Attach);
    assert_ne!(recovery.event_id, normalized.event_id);
    assert_eq!(recovery.generation, normalized.generation);
    assert_eq!(recovery.control, normalized.control);
    assert_eq!(recovery.version, normalized.version);
    assert_eq!(recovery.note_id, note.id);
    // A late ACK from before recovery must not retire the replacement event.
    acknowledge_sync_event(&ctx, &original.acknowledgement())
        .await
        .unwrap();
    assert_eq!(sync_status(&ctx).await.unwrap().pending_notes, 1);
    release_sync_event(&ctx, &recovery.event_id).await.unwrap();
}

#[tokio::test]
async fn probes_do_not_disclose_unassociated_heads_and_reject_duplicate_requests() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            labels: vec![],
            ..input("private")
        },
    )
    .await
    .unwrap();
    let source = save_note(&ctx, input("selected")).await.unwrap();
    let mut descriptor = sync_candidates(&ctx, None, 32).await.unwrap()[0]
        .descriptor
        .clone();
    assert_eq!(descriptor.note_id, source.id);
    descriptor.note_id = note.id;
    let mut query = SyncStateQuery {
        query_id: uuid::Uuid::new_v4().to_string(),
        items: vec![descriptor.clone()],
    };
    let state = query_sync_state(&ctx, &query).await.unwrap();
    assert_eq!(state.items[0].association, SyncAssociation::Missing);
    assert!(state.items[0].member.is_none() && state.items[0].head.is_none());
    query.items.push(descriptor);
    assert!(query_sync_state(&ctx, &query).await.is_err());
}

#[tokio::test]
async fn probe_checks_actual_attachment_objects_and_stale_candidates_cannot_recover() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let mut value = input("immutable file");
    value.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: String::new(),
        content: b"file bytes".to_vec(),
        storage: None,
    });
    let note = save_note(&ctx, value).await.unwrap();
    let event = next_sync_event(&ctx).await.unwrap().unwrap();
    acknowledge_sync_event(&ctx, &event.acknowledgement())
        .await
        .unwrap();
    let descriptor = sync_candidates(&ctx, None, 32).await.unwrap()[0]
        .descriptor
        .clone();
    let query = SyncStateQuery {
        query_id: uuid::Uuid::new_v4().to_string(),
        items: vec![descriptor.clone()],
    };
    let available = query_sync_state(&ctx, &query).await.unwrap();
    assert!(available.items[0].matches_snapshot(&descriptor));
    let object = note.attachments[0].storage.as_ref().unwrap();
    ctx.attachments()
        .delete_object(&object.object_key)
        .await
        .unwrap();
    assert!(!query_sync_state(&ctx, &query).await.unwrap().items[0].matches_snapshot(&descriptor));
    update_note_fields(
        &ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: note.revision,
            title: note.title,
            content: "replacement".into(),
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap();
    assert!(!sync_candidate_current(&ctx, &descriptor, true)
        .await
        .unwrap());
    assert!(!recover_missing_sync_association(&ctx, &descriptor)
        .await
        .unwrap());
    assert_eq!(sync_status(&ctx).await.unwrap().pending_notes, 1);
}

#[tokio::test]
async fn detached_state_probe_does_not_disclose_independent_private_edits() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("shared")).await.unwrap();
    let attach = next_sync_event(&ctx).await.unwrap().unwrap();
    acknowledge_sync_event(&ctx, &attach.acknowledgement())
        .await
        .unwrap();
    let detached = update_note_fields(
        &ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: note.revision,
            title: note.title,
            content: "frozen closure".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let detach = next_sync_event(&ctx).await.unwrap().unwrap();
    update_note_fields(
        &ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: detached.revision,
            title: detached.title,
            content: "independent private edit".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let query = SyncStateQuery {
        query_id: uuid::Uuid::new_v4().to_string(),
        items: vec![attach.acknowledgement()],
    };
    for expected_kind in [SyncEventKind::Detach, SyncEventKind::Fence] {
        let state = query_sync_state(&ctx, &query).await.unwrap();
        assert!(state.matches_query(&query));
        assert_eq!(state.items[0].association, SyncAssociation::Detached);
        assert_eq!(state.items[0].member.as_ref().unwrap().kind, expected_kind);
        assert!(
            state.items[0].head.is_none(),
            "private head must stay hidden"
        );
        assert!(
            !recover_missing_sync_association(&ctx, &detach.acknowledgement())
                .await
                .unwrap()
        );
        acknowledge_sync_event(&ctx, &detach.acknowledgement())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn missing_receiver_membership_recovers_pending_and_nonpending_snapshots() {
    for pending in [false, true] {
        let (sender, _sender_backend, _sender_dir) = support::test_context().await;
        let (receiver, _receiver_backend, _receiver_dir) = support::test_context().await;
        enabled(&sender).await;
        enabled(&receiver).await;
        let note = save_note(&sender, input("initial")).await.unwrap();
        let attach = next_sync_event(&sender).await.unwrap().unwrap();
        acknowledge_sync_event(&sender, &attach.acknowledgement())
            .await
            .unwrap();
        if pending {
            update_note_fields(
                &sender,
                &note.id,
                UpdateNoteFieldsInput {
                    expected_revision: note.revision,
                    title: note.title,
                    content: "pending replacement".into(),
                    labels: vec![("project".into(), "bear_note".into())],
                },
            )
            .await
            .unwrap();
        }
        let candidate = sync_current_candidate(&sender, &note.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(candidate.pending, pending);
        assert_eq!(candidate.descriptor.kind, SyncEventKind::Snapshot);
        let snapshot = sync_candidate_event(&sender, &candidate.descriptor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            apply_sync_event(&receiver, &snapshot, &HashMap::new())
                .await
                .unwrap(),
            SyncApplyResult::Ignored
        );
        let query = SyncStateQuery {
            query_id: uuid::Uuid::new_v4().to_string(),
            items: vec![candidate.descriptor.clone()],
        };
        assert_eq!(
            query_sync_state(&receiver, &query).await.unwrap().items[0].association,
            SyncAssociation::Missing,
            "ignored Snapshot cannot establish membership"
        );
        assert!(
            recover_missing_sync_association(&sender, &candidate.descriptor)
                .await
                .unwrap()
        );
        let recovery = next_sync_event(&sender).await.unwrap().unwrap();
        assert_eq!(recovery.kind, SyncEventKind::Attach);
        assert_ne!(recovery.event_id, candidate.descriptor.event_id);
        assert_eq!(recovery.generation, snapshot.generation);
        assert_eq!(recovery.control, snapshot.control);
        assert_eq!(recovery.version, snapshot.version);
        assert_eq!(recovery.snapshot, snapshot.snapshot);
        assert_eq!(recovery.snapshot_hash, snapshot.snapshot_hash);
        assert_eq!(
            apply_sync_event(&receiver, &recovery, &HashMap::new())
                .await
                .unwrap(),
            SyncApplyResult::Applied
        );
        assert!(query_sync_state(&receiver, &query).await.unwrap().items[0]
            .matches_snapshot(&candidate.descriptor));
        assert_eq!(
            get_note(&receiver, &note.id)
                .await
                .unwrap()
                .unwrap()
                .content,
            if pending {
                "pending replacement"
            } else {
                "initial"
            }
        );
        acknowledge_sync_event(&sender, &snapshot.acknowledgement())
            .await
            .unwrap();
        assert_eq!(sync_status(&sender).await.unwrap().pending_notes, 1);
        acknowledge_sync_event(&sender, &recovery.acknowledgement())
            .await
            .unwrap();
        assert_eq!(sync_status(&sender).await.unwrap().pending_notes, 0);
    }
}

#[tokio::test]
async fn missing_association_recovery_cannot_override_a_newer_pending_snapshot() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("initial")).await.unwrap();
    let attach = next_sync_event(&ctx).await.unwrap().unwrap();
    acknowledge_sync_event(&ctx, &attach.acknowledgement())
        .await
        .unwrap();
    let captured = sync_current_candidate(&ctx, &note.id)
        .await
        .unwrap()
        .unwrap();
    update_note_fields(
        &ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: note.revision,
            title: note.title,
            content: "newer pending".into(),
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap();
    let newer = sync_current_candidate(&ctx, &note.id)
        .await
        .unwrap()
        .unwrap();
    assert!(newer.pending);
    assert!(
        !recover_missing_sync_association(&ctx, &captured.descriptor)
            .await
            .unwrap()
    );
    let preserved = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(preserved.acknowledgement(), newer.descriptor);
    assert_eq!(preserved.kind, SyncEventKind::Snapshot);
    assert_eq!(preserved.snapshot.unwrap().content, "newer pending");
    release_sync_event(&ctx, &preserved.event_id).await.unwrap();
}

#[tokio::test]
async fn missing_association_recovery_cannot_override_a_receiver_fence() {
    let (sender, _sender_backend, _sender_dir) = support::test_context().await;
    let (receiver, _receiver_backend, _receiver_dir) = support::test_context().await;
    enabled(&sender).await;
    enabled(&receiver).await;
    let note = save_note(&sender, input("shared")).await.unwrap();
    let attach = next_sync_event(&sender).await.unwrap().unwrap();
    acknowledge_sync_event(&sender, &attach.acknowledgement())
        .await
        .unwrap();
    let captured = sync_current_candidate(&sender, &note.id)
        .await
        .unwrap()
        .unwrap();
    let query = SyncStateQuery {
        query_id: uuid::Uuid::new_v4().to_string(),
        items: vec![captured.descriptor.clone()],
    };
    assert_eq!(
        query_sync_state(&receiver, &query).await.unwrap().items[0].association,
        SyncAssociation::Missing
    );
    // A closure may arrive after the missing-state observation but before recovery.
    apply_sync_event(&receiver, &attach, &HashMap::new())
        .await
        .unwrap();
    let received = get_note(&receiver, &note.id).await.unwrap().unwrap();
    update_note_fields(
        &receiver,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: received.revision,
            title: received.title,
            content: "closed independent copy".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let detach = next_sync_event(&receiver).await.unwrap().unwrap();
    acknowledge_sync_event(&receiver, &detach.acknowledgement())
        .await
        .unwrap();
    let fence = sync_current_candidate(&receiver, &note.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fence.descriptor.kind, SyncEventKind::Fence);
    assert!(
        recover_missing_sync_association(&sender, &captured.descriptor)
            .await
            .unwrap()
    );
    let recovery = next_sync_event(&sender).await.unwrap().unwrap();
    assert_eq!(recovery.generation, attach.generation);
    assert_eq!(recovery.control, attach.control);
    assert_eq!(
        apply_sync_event(&receiver, &recovery, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Ignored
    );
    let state = query_sync_state(&receiver, &query).await.unwrap();
    assert_eq!(state.items[0].association, SyncAssociation::Detached);
    assert_eq!(state.items[0].member.as_ref(), Some(&fence.descriptor));
    assert!(!state.items[0].matches_snapshot(&captured.descriptor));
    assert_eq!(
        get_note(&receiver, &note.id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "closed independent copy"
    );
    acknowledge_sync_event(&sender, &recovery.acknowledgement())
        .await
        .unwrap();
    let state = query_sync_state(&receiver, &query).await.unwrap();
    assert_eq!(state.items[0].association, SyncAssociation::Detached);
    assert!(!state.items[0].matches_snapshot(&captured.descriptor));
}

#[test]
fn state_query_rejects_nonadvancing_closures_and_reused_event_ids() {
    let stamp = Hlc {
        millis: 1,
        counter: 0,
        node_id: uuid::Uuid::new_v4().to_string(),
    };
    let descriptor = SyncAck {
        event_id: uuid::Uuid::new_v4().to_string(),
        note_id: uuid::Uuid::new_v4().to_string(),
        generation: stamp.clone(),
        control: stamp.clone(),
        kind: SyncEventKind::Snapshot,
        version: stamp,
        snapshot_hash: "a".repeat(64),
    };
    let mut query = SyncStateQuery {
        query_id: uuid::Uuid::new_v4().to_string(),
        items: vec![descriptor.clone()],
    };
    for kind in [SyncEventKind::Detach, SyncEventKind::Fence] {
        query.items[0].kind = kind;
        assert!(validate_sync_state_query(&query).is_err(), "{kind:?}");
        query.items[0].control.counter += 1;
        assert!(validate_sync_state_query(&query).is_ok());
        query.items[0].control = descriptor.control.clone();
    }
    query.items[0] = descriptor.clone();
    let mut duplicate = descriptor;
    duplicate.note_id = uuid::Uuid::new_v4().to_string();
    query.items.push(duplicate);
    assert!(validate_sync_state_query(&query).is_err());
    query.items[1].event_id = uuid::Uuid::new_v4().to_string();
    assert!(validate_sync_state_query(&query).is_ok());
}

#[tokio::test]
async fn received_attach_normalizes_without_replay_and_preserves_exact_retry_receipt() {
    let (sender, _sender_backend, _sender_dir) = support::test_context().await;
    let (receiver, receiver_backend, _receiver_dir) = support::test_context().await;
    enabled(&sender).await;
    enabled(&receiver).await;
    let bytes = b"received attachment".to_vec();
    let mut value = input("received snapshot");
    value.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: String::new(),
        content: bytes.clone(),
        storage: None,
    });
    let note = save_note(&sender, value).await.unwrap();
    let attach = next_sync_event(&sender).await.unwrap().unwrap();
    let attachments = HashMap::from([("file".into(), bytes)]);
    assert_eq!(
        apply_sync_event(&receiver, &attach, &attachments)
            .await
            .unwrap(),
        SyncApplyResult::Applied
    );
    let normalized = sync_current_candidate(&receiver, &note.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!normalized.pending);
    assert_eq!(normalized.descriptor.kind, SyncEventKind::Snapshot);
    assert_ne!(normalized.descriptor.event_id, attach.event_id);
    assert_eq!(normalized.descriptor.generation, attach.generation);
    assert_eq!(normalized.descriptor.control, attach.control);
    assert_eq!(normalized.descriptor.version, attach.version);
    assert_eq!(normalized.descriptor.snapshot_hash, attach.snapshot_hash);
    let received: SyncEvent = serde_json::from_value(
        receiver_backend
            .session()
            .await
            .unwrap()
            .get_sync_record("received", &attach.event_id)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(received, attach, "receipt keeps the exact wire event");
    assert_eq!(
        apply_sync_event(&receiver, &attach, &attachments)
            .await
            .unwrap(),
        SyncApplyResult::Duplicate
    );
    assert_eq!(
        sync_current_candidate(&receiver, &note.id)
            .await
            .unwrap()
            .unwrap()
            .descriptor,
        normalized.descriptor
    );
    acknowledge_sync_event(&sender, &received.acknowledgement())
        .await
        .unwrap();
    assert_eq!(sync_status(&sender).await.unwrap().pending_notes, 0);
    let query = SyncStateQuery {
        query_id: uuid::Uuid::new_v4().to_string(),
        items: vec![normalized.descriptor.clone()],
    };
    assert!(query_sync_state(&sender, &query).await.unwrap().items[0]
        .matches_snapshot(&normalized.descriptor));
    assert!(
        sync_candidate_current(&receiver, &normalized.descriptor, true)
            .await
            .unwrap()
    );
    assert!(next_sync_event(&receiver).await.unwrap().is_none());
}

#[tokio::test]
async fn losing_received_attach_keeps_its_newer_winner_attach_pending() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("local winner")).await.unwrap();
    let original = next_sync_event(&ctx).await.unwrap().unwrap();
    let mut older = original.clone();
    older.event_id = uuid::Uuid::new_v4().to_string();
    older.version.millis -= 1;
    older.snapshot.as_mut().unwrap().content = "older remote".into();
    older.snapshot_hash = canonical_snapshot_hash(older.snapshot.as_ref()).unwrap();
    assert_eq!(
        apply_sync_event(&ctx, &older, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Ignored
    );
    let winner = sync_current_candidate(&ctx, &note.id)
        .await
        .unwrap()
        .unwrap();
    assert!(winner.pending);
    assert_eq!(winner.descriptor.kind, SyncEventKind::Attach);
    assert_eq!(winner.descriptor.version, original.version);
    assert_eq!(winner.descriptor.snapshot_hash, original.snapshot_hash);
    assert_ne!(winner.descriptor.event_id, older.event_id);
    assert_eq!(
        apply_sync_event(&ctx, &older, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Duplicate
    );
    acknowledge_sync_event(&ctx, &older.acknowledgement())
        .await
        .unwrap();
    assert_eq!(
        next_sync_event(&ctx)
            .await
            .unwrap()
            .unwrap()
            .acknowledgement(),
        winner.descriptor
    );
    acknowledge_sync_event(&ctx, &winner.descriptor)
        .await
        .unwrap();
    release_sync_event(&ctx, &original.event_id).await.unwrap();
}

struct CountSnapshotTransactions {
    inner: Arc<dyn StorageBackend>,
    snapshots: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl StorageBackend for CountSnapshotTransactions {
    async fn session(&self) -> note_storage::StorageResult<Box<dyn note_storage::StorageSession>> {
        self.inner.session().await
    }
    async fn begin(
        &self,
        mode: TransactionMode,
    ) -> note_storage::StorageResult<Box<dyn note_storage::StorageTransaction>> {
        if mode == TransactionMode::Snapshot {
            self.snapshots
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.inner.begin(mode).await
    }
    async fn info(&self) -> note_storage::StorageResult<note_storage::BackendInfo> {
        self.inner.info().await
    }
}

#[tokio::test]
async fn round_candidates_capture_all_pages_and_pending_under_one_snapshot_without_file_reads() {
    let (ctx, backend, dir) = support::test_context().await;
    enabled(&ctx).await;
    let acknowledged = save_note(&ctx, input("acknowledged")).await.unwrap();
    let attach = next_sync_event(&ctx).await.unwrap().unwrap();
    acknowledge_sync_event(&ctx, &attach.acknowledgement())
        .await
        .unwrap();
    let mut value = input("pending attachment");
    value.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: String::new(),
        content: b"unreadable during capture".to_vec(),
        storage: None,
    });
    let pending = save_note(&ctx, value).await.unwrap();
    ctx.attachments()
        .delete_object(&pending.attachments[0].storage.as_ref().unwrap().object_key)
        .await
        .unwrap();
    // Retained control rows occupy another page even without live note rows.
    for index in 0..130 {
        let mut fence = attach.clone();
        fence.event_id = uuid::Uuid::new_v4().to_string();
        fence.note_id = format!("00000000-0000-4000-8000-{index:012}");
        fence.kind = SyncEventKind::Fence;
        fence.control.counter += 1;
        fence.snapshot = None;
        apply_sync_event(&ctx, &fence, &HashMap::new())
            .await
            .unwrap();
    }
    let snapshots = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let scanning = Context::new(
        Arc::new(CountSnapshotTransactions {
            inner: backend,
            snapshots: snapshots.clone(),
        }),
        ctx.embedder.clone(),
        Arc::new(::note_attachments::FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    );
    let captured = sync_round_candidates(&scanning, false).await.unwrap();
    assert_eq!(snapshots.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(captured.len(), 132);
    assert_eq!(
        captured
            .iter()
            .filter(|candidate| candidate.descriptor.kind == SyncEventKind::Fence)
            .count(),
        130
    );
    assert!(captured
        .iter()
        .any(|candidate| candidate.descriptor.note_id == acknowledged.id && !candidate.pending));
    let pending_round = sync_round_candidates(&scanning, true).await.unwrap();
    assert_eq!(snapshots.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(pending_round.len(), 1);
    assert!(pending_round[0].pending);
    assert_eq!(pending_round[0].descriptor.note_id, pending.id);
    update_note_fields(
        &ctx,
        &pending.id,
        UpdateNoteFieldsInput {
            expected_revision: pending.revision,
            title: pending.title,
            content: "replacement belongs to the next round".into(),
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap();
    save_note(&ctx, input("new note belongs to the next round"))
        .await
        .unwrap();
    assert_eq!(captured.len(), 132);
    assert_eq!(pending_round.len(), 1);
    assert!(
        !sync_candidate_current(&ctx, &pending_round[0].descriptor, false)
            .await
            .unwrap()
    );
    let next = sync_round_candidates(&scanning, false).await.unwrap();
    assert_eq!(next.len(), 133);
}

async fn contracts(ctx: &Context, backend: &Arc<dyn StorageBackend>) {
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    tx.put_sync_record("sync-test", "rollback", &serde_json::json!({"value":1}))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_sync_record("sync-test", "rollback")
        .await
        .unwrap()
        .is_none());
    enabled(ctx).await;
    let note = save_note(ctx, input("local one")).await.unwrap();
    let first = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(first.kind, SyncEventKind::Attach);
    let local = update_note_fields(
        ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: note.revision,
            title: note.title.clone(),
            content: "local two".into(),
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    acknowledge_sync_event(ctx, &first.acknowledgement())
        .await
        .unwrap();
    let second = next_sync_event(ctx).await.unwrap().unwrap();
    assert_ne!(second.event_id, first.event_id);
    assert_eq!(second.snapshot.as_ref().unwrap().content, "local two");
    let incoming = remote(second.clone(), SyncEventKind::Attach, "remote winner");
    assert_eq!(
        apply_sync_event(ctx, &incoming, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Applied
    );
    assert_eq!(
        apply_sync_event(ctx, &incoming, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Duplicate
    );
    let merged = get_note(ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(merged.id, note.id);
    assert_eq!(merged.content, "remote winner");
    assert!(merged.revision > local.revision);
    assert!(
        next_sync_event(ctx).await.unwrap().is_none(),
        "winning source version must not echo"
    );
    let mut invalid = incoming.clone();
    invalid.event_id = uuid::Uuid::new_v4().to_string();
    invalid.snapshot.as_mut().unwrap().content = "different at same stamp".into();
    invalid.snapshot_hash = canonical_snapshot_hash(invalid.snapshot.as_ref()).unwrap();
    assert!(apply_sync_event(ctx, &invalid, &HashMap::new())
        .await
        .unwrap_err()
        .to_string()
        .contains("same sync version"));
    // Losing peer snapshot gets the winning complete version queued for convergence.
    let mut old = incoming.clone();
    old.event_id = uuid::Uuid::new_v4().to_string();
    old.version.counter -= 1;
    old.snapshot.as_mut().unwrap().content = "older".into();
    old.snapshot_hash = canonical_snapshot_hash(old.snapshot.as_ref()).unwrap();
    assert_eq!(
        apply_sync_event(ctx, &old, &HashMap::new()).await.unwrap(),
        SyncApplyResult::Ignored
    );
    let response = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(response.version, incoming.version);
    acknowledge_sync_event(ctx, &response.acknowledgement())
        .await
        .unwrap();
    // Pause captures without handing events to transport.
    let mut paused = config();
    paused.sync.enabled = false;
    update_system_config(ctx, &paused).await.unwrap();
    let paused_note = update_note_fields(
        ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: merged.revision,
            title: merged.title.clone(),
            content: "paused edit".into(),
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert!(next_sync_event(ctx).await.unwrap().is_none());
    assert_eq!(sync_status(ctx).await.unwrap().pending_notes, 1);
    update_system_config(ctx, &config()).await.unwrap();
    let resumed = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(resumed.snapshot.unwrap().content, "paused edit");
    // Union: removing one matching label preserves association if another rule matches.
    let union = update_note_fields(
        ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: paused_note.revision,
            title: paused_note.title,
            content: paused_note.content,
            labels: vec![("team".into(), "notes".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let union_event = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(union_event.kind, SyncEventKind::Snapshot);
    acknowledge_sync_event(ctx, &union_event.acknowledgement())
        .await
        .unwrap();
    let detached = update_note_fields(
        ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: union.revision,
            title: union.title,
            content: "frozen detach".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let detach = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(detach.kind, SyncEventKind::Detach);
    let independent = update_note_fields(
        ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: detached.revision,
            title: detached.title,
            content: "independent edit".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        next_sync_event(ctx)
            .await
            .unwrap()
            .unwrap()
            .snapshot
            .unwrap()
            .content,
        "frozen detach"
    );
    acknowledge_sync_event(ctx, &detach.acknowledgement())
        .await
        .unwrap();
    let fence = sync_inventory(ctx, None, 100)
        .await
        .unwrap()
        .events
        .into_iter()
        .find(|e| e.note_id == note.id)
        .unwrap();
    assert_eq!(fence.kind, SyncEventKind::Fence);
    assert!(fence.snapshot.is_none());
    let rejoined = update_note_fields(
        ctx,
        &note.id,
        UpdateNoteFieldsInput {
            expected_revision: independent.revision,
            title: independent.title,
            content: independent.content,
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let reentry = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(reentry.kind, SyncEventKind::Attach);
    assert!(reentry.generation > detach.generation);
    acknowledge_sync_event(ctx, &detach.acknowledgement())
        .await
        .unwrap();
    assert_eq!(
        next_sync_event(ctx).await.unwrap().unwrap().event_id,
        reentry.event_id
    );
    assert_eq!(
        apply_sync_event(ctx, &fence, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Ignored
    );
    let mut late = remote(
        detach.clone(),
        SyncEventKind::Snapshot,
        "late old generation",
    );
    late.version.millis += 1_000_000;
    assert_eq!(
        apply_sync_event(ctx, &late, &HashMap::new()).await.unwrap(),
        SyncApplyResult::Ignored
    );
    delete_note(ctx, &note.id, rejoined.revision).await.unwrap();
    let deleted = next_sync_event(ctx).await.unwrap().unwrap();
    assert!(deleted.snapshot.unwrap().deleted_at.is_some());
    let deleted_note = backend
        .session()
        .await
        .unwrap()
        .get_sync_note(&note.id)
        .await
        .unwrap()
        .unwrap();
    restore_notes(
        ctx,
        &[RestoreNoteInput {
            id: note.id.clone(),
            expected_revision: deleted_note.revision,
        }],
    )
    .await
    .unwrap();
    let restored = next_sync_event(ctx).await.unwrap().unwrap();
    assert!(restored.snapshot.unwrap().deleted_at.is_none());
    let restored_note = get_note(ctx, &note.id).await.unwrap().unwrap();
    delete_note(ctx, &note.id, restored_note.revision)
        .await
        .unwrap();
    let purging = backend
        .session()
        .await
        .unwrap()
        .get_sync_note(&note.id)
        .await
        .unwrap()
        .unwrap();
    permanently_delete_note(ctx, &note.id, purging.revision)
        .await
        .unwrap();
    let purge = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(purge.kind, SyncEventKind::Purge);
    assert!(purge.snapshot.is_none());
    let mut revive = reentry;
    revive.event_id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        apply_sync_event(ctx, &revive, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Ignored
    );
    let response = next_sync_event(ctx).await.unwrap().unwrap();
    assert_eq!(response.kind, SyncEventKind::Purge);
    assert_eq!(response.version, purge.version);
    // Validate the generated tombstone response through a second real storage backend.
    let (receiver, _storage, _dir) = support::test_context().await;
    enabled(&receiver).await;
    apply_sync_event(&receiver, &revive, &HashMap::new())
        .await
        .unwrap();
    assert_eq!(
        apply_sync_event(&receiver, &response, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Applied
    );
    acknowledge_sync_event(ctx, &response.acknowledgement())
        .await
        .unwrap();
    assert!(get_note(&receiver, &note.id).await.unwrap().is_none());
    assert!(get_note(ctx, &note.id).await.unwrap().is_none());
}
#[tokio::test]
async fn turso_snapshot_replication_contracts() {
    let (ctx, backend, _dir) = support::test_context().await;
    contracts(&ctx, &backend).await;
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL live PostgreSQL"]
async fn postgres_snapshot_replication_contracts() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let admin = ::sqlx::PgPool::connect(&url).await.unwrap();
    // Schema identifier contains only this literal prefix and generated UUID hex digits.
    let schema = format!("sync_{}", uuid::Uuid::new_v4().simple());
    ::sqlx::query(::sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut isolated_url = url::Url::parse(&url).unwrap();
    isolated_url
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema},public"));
    let concrete = Arc::new(
        note_storage_pg::PgStorage::connect(isolated_url.as_str(), 5)
            .await
            .unwrap(),
    );
    let backend: Arc<dyn StorageBackend> = concrete.clone();
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::new(
        backend.clone(),
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(::note_attachments::FilesystemAttachmentStore::new(
            dir.path().to_path_buf(),
        )),
    );
    contracts(&ctx, &backend).await;
    drop(ctx);
    drop(backend);
    concrete.close().await;
    ::sqlx::query(::sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA {schema} CASCADE"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
}
#[tokio::test]
async fn bootstrap_and_rule_changes_preserve_historical_version() {
    let (ctx, backend, _dir) = support::test_context().await;
    let id = uuid::Uuid::new_v4().to_string();
    let session = backend.session().await.unwrap();
    session
        .insert_label_key_if_missing("project", "")
        .await
        .unwrap();
    session
        .insert_note(NewNote {
            id: &id,
            title: "Historical",
            content: "old",
            attachments: &[],
            created_at: 90,
            updated_at: 100,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    session
        .attach_label(&id, "project", "bear_note")
        .await
        .unwrap();
    enabled(&ctx).await;
    let before = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(before.version.millis, 100_000);
    acknowledge_sync_event(&ctx, &before.acknowledgement())
        .await
        .unwrap();
    let mut removed = config();
    removed.sync.rules.clear();
    update_system_config(&ctx, &removed).await.unwrap();
    let detach = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(detach.version, before.version);
    update_system_config(&ctx, &config()).await.unwrap();
    let attach = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(attach.version, before.version);
    assert!(attach.generation > detach.generation);
}
#[tokio::test]
async fn old_read_lease_pins_attachment_independently_of_latest_queue() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let mut initial = input("attachment one");
    initial.attachments = vec![NoteAttachment {
        id: "file".into(),
        path: "./image.png".into(),
        mime: "image/png".into(),
        description: "".into(),
        content: b"one".to_vec(),
        storage: None,
    }];
    let note = save_note(&ctx, initial).await.unwrap();
    let event = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(
        event.snapshot.as_ref().unwrap().attachments[0].path,
        "image.png"
    );
    let key = note.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    let mut replacement = input("attachment two");
    replacement.attachments = vec![NoteAttachment {
        id: "file".into(),
        path: "image.png".into(),
        mime: "image/png".into(),
        description: "".into(),
        content: b"two".to_vec(),
        storage: None,
    }];
    update_note(&ctx, &note.id, note.revision, replacement)
        .await
        .unwrap();
    assert!(
        !reserve_sync_attachment_cleanup(&ctx, &note.id, &key, false)
            .await
            .unwrap()
    );
    assert_eq!(
        read_sync_attachment(&ctx, &event, "file", 3).await.unwrap(),
        b"one"
    );
    assert!(read_sync_attachment(&ctx, &event, "file", 2).await.is_err());
    release_sync_event(&ctx, &event.event_id).await.unwrap();
    assert!(reserve_sync_attachment_cleanup(&ctx, &note.id, &key, false)
        .await
        .unwrap());
    assert!(
        !reserve_sync_event(&ctx, &event).await.unwrap(),
        "superseded inventory event cannot reacquire pin"
    );
    let current = next_sync_event(&ctx).await.unwrap().unwrap();
    acknowledge_sync_event(&ctx, &current.acknowledgement())
        .await
        .unwrap();
    let inventory = sync_inventory(&ctx, None, 100)
        .await
        .unwrap()
        .events
        .into_iter()
        .find(|e| e.note_id == note.id)
        .unwrap();
    assert!(reserve_sync_event(&ctx, &inventory).await.unwrap());
    assert_eq!(
        read_sync_attachment(&ctx, &inventory, "file", 10)
            .await
            .unwrap(),
        b"two"
    );
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    tx.delete_sync_record("pending", &note.id).await.unwrap();
    tx.rollback().await.unwrap();
}
#[tokio::test]
async fn peer_identity_and_unknown_snapshot_are_fenced() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    pin_sync_peer(&ctx, "gao_note", "22222222-2222-4222-8222-222222222222")
        .await
        .unwrap();
    assert!(
        pin_sync_peer(&ctx, "gao_note", "33333333-3333-4333-8333-333333333333")
            .await
            .is_err()
    );
    let note = save_note(&ctx, input("one")).await.unwrap();
    let mut event = next_sync_event(&ctx).await.unwrap().unwrap();
    event.note_id = uuid::Uuid::new_v4().to_string();
    event.kind = SyncEventKind::Snapshot;
    event.event_id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        apply_sync_event(&ctx, &event, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Ignored
    );
    assert!(get_note(&ctx, &event.note_id).await.unwrap().is_none());
    assert!(get_note(&ctx, &note.id).await.unwrap().is_some());
}

#[test]
fn cross_language_canonical_snapshot_hash_vector() {
    let snapshot = SyncSnapshot {
        title: "中文".into(),
        content: "# Note\nhello".into(),
        labels: vec![SyncLabel {
            key: "project".into(),
            value: "bear_note".into(),
            description: "".into(),
            value_type: "text".into(),
        }],
        created_at: 1,
        updated_at: 2,
        deleted_at: None,
        attachments: vec![],
    };
    assert_eq!(
        canonical_snapshot_hash(Some(&snapshot)).unwrap(),
        "e16e6a9d800154b9025b788e467934a03bec054e8fddab1fe19348ec19d60f17"
    );
    assert_eq!(
        canonical_snapshot_hash(None).unwrap(),
        "74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b"
    );
}
#[tokio::test]
async fn pending_scheduler_progresses_and_stale_crash_leases_are_reclaimed() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    save_note(&ctx, input("one")).await.unwrap();
    save_note(&ctx, input("two")).await.unwrap();
    let first = next_sync_event(&ctx).await.unwrap().unwrap();
    let second = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_ne!(first.note_id, second.note_id);
    assert_eq!(
        next_sync_event(&ctx).await.unwrap().unwrap().note_id,
        first.note_id
    );
    release_abandoned_sync_events(&ctx).await.unwrap();
    assert!(read_sync_attachment(&ctx, &first, "absent", 10)
        .await
        .unwrap_err()
        .to_string()
        .contains("read lease"));
    assert_eq!(sync_status(&ctx).await.unwrap().pending_notes, 2);
}
#[test]
fn independently_generated_receiver_hash_fixture_matches() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/sync/hash-vectors-v1.json")).unwrap();
    for vector in fixtures["vectors"].as_array().unwrap() {
        let snapshot: Option<SyncSnapshot> =
            serde_json::from_value(vector["snapshot"].clone()).unwrap();
        assert_eq!(
            canonical_snapshot_hash(snapshot.as_ref()).unwrap(),
            vector["sha256"].as_str().unwrap()
        );
    }
}
#[tokio::test]
async fn inventory_only_detach_ack_retires_bytes_and_profile_is_immutable() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("one")).await.unwrap();
    let attach = next_sync_event(&ctx).await.unwrap().unwrap();
    acknowledge_sync_event(&ctx, &attach.acknowledgement())
        .await
        .unwrap();
    let mut detach = remote(attach, SyncEventKind::Detach, "remote detach");
    detach.control.counter += 1;
    detach.snapshot.as_mut().unwrap().labels.clear();
    detach.snapshot_hash = canonical_snapshot_hash(detach.snapshot.as_ref()).unwrap();
    assert_eq!(
        apply_sync_event(&ctx, &detach, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Applied
    );
    assert!(next_sync_event(&ctx).await.unwrap().is_none());
    assert!(reserve_sync_event(&ctx, &detach).await.unwrap());
    acknowledge_sync_event(&ctx, &detach.acknowledgement())
        .await
        .unwrap();
    let inventory = sync_inventory(&ctx, None, 100).await.unwrap();
    let fence = inventory
        .events
        .iter()
        .find(|e| e.note_id == note.id)
        .unwrap();
    assert_eq!(fence.kind, SyncEventKind::Fence);
    let member = backend
        .session()
        .await
        .unwrap()
        .get_sync_record("member", &note.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member["attachments"], serde_json::json!([]));
    pin_sync_peer(&ctx, "gao_note", "44444444-4444-4444-8444-444444444444")
        .await
        .unwrap();
    let mut changed = config();
    changed.sync.connection_profile = Some("other".into());
    assert!(update_system_config(&ctx, &changed)
        .await
        .unwrap_err()
        .to_string()
        .contains("cannot change"));
}
#[tokio::test]
async fn synchronization_rejects_uri_schemes_without_restricting_regular_notes() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("one")).await.unwrap();
    let base = next_sync_event(&ctx).await.unwrap().unwrap();
    for path in ["mailto:x", "file:x", "ftp+tls:x"] {
        let mut event = remote(base.clone(), SyncEventKind::Attach, "remote");
        event.snapshot.as_mut().unwrap().attachments = vec![SyncAttachment {
            id: "a".into(),
            path: path.into(),
            mime: "text/plain".into(),
            description: "".into(),
            size_bytes: 0,
            checksum_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .into(),
        }];
        event.snapshot_hash = canonical_snapshot_hash(event.snapshot.as_ref()).unwrap();
        assert!(
            apply_sync_event(&ctx, &event, &HashMap::from([("a".into(), vec![])]))
                .await
                .is_err()
        );
    }
    assert_eq!(
        get_note(&ctx, &note.id).await.unwrap().unwrap().content,
        "one"
    );
}

#[tokio::test]
async fn profile_can_be_corrected_before_peer_registration_without_retiming_content() {
    let (ctx, _backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("one")).await.unwrap();
    let first = next_sync_event(&ctx).await.unwrap().unwrap();
    let mut corrected = config();
    corrected.sync.connection_profile = Some("corrected".into());
    update_system_config(&ctx, &corrected).await.unwrap();
    let after = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(after.note_id, note.id);
    assert_eq!(after.version, first.version);
    assert!(after.generation > first.generation);
    acknowledge_sync_event(&ctx, &first.acknowledgement())
        .await
        .unwrap();
    assert_eq!(
        next_sync_event(&ctx).await.unwrap().unwrap().event_id,
        after.event_id
    );
}
async fn mutation_family_contracts(ctx: &Context) {
    enabled(&ctx).await;
    let id = uuid::Uuid::new_v4().to_string();
    import_data(
        &ctx,
        ExportData {
            version: 1,
            label_keys: vec![ExportLabelKey {
                key: "project".into(),
                description: "Project".into(),
                value_type: "text".into(),
            }],
            notes: vec![ExportNote {
                id: id.clone(),
                title: "Imported".into(),
                content: "original".into(),
                attachments: vec![],
                created_at: 10,
                updated_at: 20,
                deleted_at: None,
                labels: vec![("project".into(), "bear_note".into())],
            }],
        },
    )
    .await
    .unwrap();
    let imported = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(imported.version.millis, 20_000);
    acknowledge_sync_event(&ctx, &imported.acknowledgement())
        .await
        .unwrap();
    update_label_key(&ctx, "project", "Catalog changed")
        .await
        .unwrap();
    let catalog = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(
        catalog.snapshot.as_ref().unwrap().labels[0].description,
        "Catalog changed"
    );
    assert!(catalog.version > imported.version);
    acknowledge_sync_event(&ctx, &catalog.acknowledgement())
        .await
        .unwrap();
    bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "project=bear_note".into(),
            set: vec![("team".into(), "notes".into())],
            remove: vec!["project".into()],
        },
    )
    .await
    .unwrap();
    let bulk = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(bulk.kind, SyncEventKind::Snapshot);
    assert_eq!(bulk.snapshot.as_ref().unwrap().labels[0].key, "team");
    acknowledge_sync_event(&ctx, &bulk.acknowledgement())
        .await
        .unwrap();
    let current = get_note_metadata(&ctx, &id).await.unwrap().unwrap();
    let added = put_note_attachment(
        &ctx,
        &id,
        current.revision,
        NoteAttachment {
            id: "file".into(),
            path: "empty.bin".into(),
            mime: "application/octet-stream".into(),
            description: "Empty".into(),
            content: vec![],
            storage: None,
        },
    )
    .await
    .unwrap();
    let attachment = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(
        attachment.snapshot.as_ref().unwrap().attachments[0].size_bytes,
        0
    );
    assert!(read_sync_attachment(&ctx, &attachment, "file", 0)
        .await
        .unwrap()
        .is_empty());
    acknowledge_sync_event(&ctx, &attachment.acknowledgement())
        .await
        .unwrap();
    delete_note_attachment(&ctx, &id, "file", added.revision)
        .await
        .unwrap();
    let removed = next_sync_event(&ctx).await.unwrap().unwrap();
    assert!(removed.snapshot.as_ref().unwrap().attachments.is_empty());
    acknowledge_sync_event(&ctx, &removed.acknowledgement())
        .await
        .unwrap();
    delete_label_key(&ctx, "team").await.unwrap();
    assert_eq!(
        next_sync_event(&ctx).await.unwrap().unwrap().kind,
        SyncEventKind::Detach
    );
}

async fn assert_no_pending(ctx: &Context) {
    assert!(next_sync_event(ctx).await.unwrap().is_none());
}
#[tokio::test]
async fn closed_matching_copy_requires_label_or_rule_reentry_before_syncing() {
    for closing_kind in [SyncEventKind::Fence, SyncEventKind::Detach] {
        let (ctx, backend, _dir) = support::test_context().await;
        enabled(&ctx).await;
        let note = save_note(&ctx, input("local winner")).await.unwrap();
        let initial = next_sync_event(&ctx).await.unwrap().unwrap();
        acknowledge_sync_event(&ctx, &initial.acknowledgement())
            .await
            .unwrap();
        let mut close = initial.clone();
        close.event_id = uuid::Uuid::new_v4().to_string();
        close.kind = closing_kind;
        close.control.counter += 1;
        if closing_kind == SyncEventKind::Fence {
            close.snapshot = None;
        } else {
            close.version.millis -= 1;
            close.snapshot.as_mut().unwrap().content = "losing detach".into();
            close.snapshot_hash = canonical_snapshot_hash(close.snapshot.as_ref()).unwrap();
        }
        apply_sync_event(&ctx, &close, &HashMap::new())
            .await
            .unwrap();
        if let Some(out) = next_sync_event(&ctx).await.unwrap() {
            acknowledge_sync_event(&ctx, &out.acknowledgement())
                .await
                .unwrap();
        }
        let current = get_note_metadata(&ctx, &note.id).await.unwrap().unwrap();
        let independently_edited = update_note_fields(
            &ctx,
            &note.id,
            UpdateNoteFieldsInput {
                expected_revision: current.revision,
                title: current.title.clone(),
                content: "independent body edit".into(),
                labels: vec![("project".into(), "bear_note".into())],
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_no_pending(&ctx).await;
        // A global pause/resume is not a selector re-entry.
        let mut paused = config();
        paused.sync.enabled = false;
        update_system_config(&ctx, &paused).await.unwrap();
        update_system_config(&ctx, &config()).await.unwrap();
        assert_no_pending(&ctx).await;
        let member = backend
            .session()
            .await
            .unwrap()
            .get_sync_record("member", &note.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(member["attached"], false);
        let unselected = update_note_fields(
            &ctx,
            &note.id,
            UpdateNoteFieldsInput {
                expected_revision: independently_edited.revision,
                title: independently_edited.title,
                content: independently_edited.content,
                labels: vec![],
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_no_pending(&ctx).await;
        update_note_fields(
            &ctx,
            &note.id,
            UpdateNoteFieldsInput {
                expected_revision: unselected.revision,
                title: unselected.title,
                content: unselected.content,
                labels: vec![("project".into(), "bear_note".into())],
            },
        )
        .await
        .unwrap();
        let reentry = next_sync_event(&ctx).await.unwrap().unwrap();
        assert_eq!(reentry.kind, SyncEventKind::Attach);
        assert!(reentry.generation > close.generation);
        acknowledge_sync_event(&ctx, &reentry.acknowledgement())
            .await
            .unwrap();
        let mut close_again = reentry.clone();
        close_again.event_id = uuid::Uuid::new_v4().to_string();
        close_again.kind = SyncEventKind::Fence;
        close_again.control.counter += 1;
        close_again.snapshot = None;
        apply_sync_event(&ctx, &close_again, &HashMap::new())
            .await
            .unwrap();
        let mut no_rules = config();
        no_rules.sync.rules.clear();
        update_system_config(&ctx, &no_rules).await.unwrap();
        assert_no_pending(&ctx).await;
        update_system_config(&ctx, &config()).await.unwrap();
        let rule_reentry = next_sync_event(&ctx).await.unwrap().unwrap();
        assert_eq!(rule_reentry.kind, SyncEventKind::Attach);
        assert!(rule_reentry.generation > reentry.generation);
    }
}
#[tokio::test]
async fn archive_recreation_supersedes_purge_with_fresh_version_and_generation() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let note = save_note(&ctx, input("archived content")).await.unwrap();
    let archive = export_data(&ctx).await.unwrap();
    let attach = next_sync_event(&ctx).await.unwrap().unwrap();
    acknowledge_sync_event(&ctx, &attach.acknowledgement())
        .await
        .unwrap();
    delete_note(&ctx, &note.id, note.revision).await.unwrap();
    let deleted = backend
        .session()
        .await
        .unwrap()
        .get_sync_note(&note.id)
        .await
        .unwrap()
        .unwrap();
    permanently_delete_note(&ctx, &note.id, deleted.revision)
        .await
        .unwrap();
    let purge = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(purge.kind, SyncEventKind::Purge);
    import_data(&ctx, archive).await.unwrap();
    let recreated = next_sync_event(&ctx).await.unwrap().unwrap();
    assert_eq!(recreated.kind, SyncEventKind::Attach);
    assert!(recreated.version > purge.version);
    assert!(recreated.generation > purge.generation);
    assert_eq!(
        recreated.snapshot.as_ref().unwrap().content,
        "archived content"
    );
    validate_sync_event(&recreated, &HashMap::new()).unwrap();
    acknowledge_sync_event(&ctx, &purge.acknowledgement())
        .await
        .unwrap();
    assert_eq!(
        next_sync_event(&ctx).await.unwrap().unwrap().event_id,
        recreated.event_id
    );
    let (receiver, _other, _dir) = support::test_context().await;
    enabled(&receiver).await;
    apply_sync_event(&receiver, &attach, &HashMap::new())
        .await
        .unwrap();
    apply_sync_event(&receiver, &purge, &HashMap::new())
        .await
        .unwrap();
    assert!(get_note_metadata(&receiver, &note.id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        apply_sync_event(&receiver, &recreated, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Applied
    );
    assert_eq!(
        get_note_metadata(&receiver, &note.id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "archived content"
    );
}
#[tokio::test]
async fn winning_description_conflict_is_visible_and_atomic_matching_description_applies() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    define_label_key(&ctx, "project", "Existing global description")
        .await
        .unwrap();
    let note = save_note(&ctx, input("local")).await.unwrap();
    let original = next_sync_event(&ctx).await.unwrap().unwrap();
    let head_before = backend
        .session()
        .await
        .unwrap()
        .get_sync_record("head", &note.id)
        .await
        .unwrap();
    let mut incoming = remote(original.clone(), SyncEventKind::Attach, "remote winner");
    incoming.snapshot.as_mut().unwrap().labels[0].description =
        "Conflicting remote description".into();
    incoming.snapshot_hash = canonical_snapshot_hash(incoming.snapshot.as_ref()).unwrap();
    let error = apply_sync_event(&ctx, &incoming, &HashMap::new())
        .await
        .unwrap_err();
    assert_eq!(sync_note_rejection_reason(&error), Some("catalog_conflict"));
    assert!(error
        .to_string()
        .contains("incompatible catalog description"));
    assert_eq!(
        get_note_metadata(&ctx, &note.id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "local"
    );
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_sync_record("head", &note.id)
            .await
            .unwrap(),
        head_before
    );
    incoming.snapshot.as_mut().unwrap().labels[0].description =
        "Existing global description".into();
    incoming.snapshot_hash = canonical_snapshot_hash(incoming.snapshot.as_ref()).unwrap();
    assert_eq!(
        apply_sync_event(&ctx, &incoming, &HashMap::new())
            .await
            .unwrap(),
        SyncApplyResult::Applied
    );
    let current = get_note_metadata(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(current.content, "remote winner");
    assert_eq!(current.labels[0].description, "Existing global description");
}

#[tokio::test]
async fn malformed_note_rejection_is_typed_but_storage_failure_is_retryable() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    save_note(&ctx, input("local")).await.unwrap();
    let event = next_sync_event(&ctx).await.unwrap().unwrap();
    let mut incoming = remote(event, SyncEventKind::Attach, "remote");
    incoming.snapshot.as_mut().unwrap().title.clear();
    incoming.snapshot_hash = canonical_snapshot_hash(incoming.snapshot.as_ref()).unwrap();
    let before = records(&backend).await;
    let error = apply_sync_event(&ctx, &incoming, &HashMap::new())
        .await
        .unwrap_err();
    assert_eq!(
        sync_note_rejection_reason(&error),
        Some("validation_failed")
    );
    assert_eq!(records(&backend).await, before);
    incoming.snapshot.as_mut().unwrap().title = "Valid".into();
    incoming.snapshot_hash = canonical_snapshot_hash(incoming.snapshot.as_ref()).unwrap();
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    tx.put_sync_record(
        "head",
        &incoming.note_id,
        &serde_json::json!({"invalid":true}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let error = apply_sync_event(&ctx, &incoming, &HashMap::new())
        .await
        .unwrap_err();
    assert!(sync_note_rejection_reason(&error).is_none());
}

#[test]
fn cleanup_failure_context_prevents_note_rejection_classification() {
    let rejection = anyhow::Error::new(SyncNoteRejection::CatalogConflict(
        "catalog mismatch".into(),
    ));
    assert_eq!(
        sync_note_rejection_reason(&rejection),
        Some("catalog_conflict")
    );
    let cleanup_failure = rejection.context("generated attachment cleanup also failed");
    assert!(sync_note_rejection_reason(&cleanup_failure).is_none());
    assert!(sync_note_rejection_reason(&anyhow::anyhow!("database unavailable")).is_none());
}

struct PauseBeforeBootstrapPage {
    inner: Arc<dyn StorageBackend>,
    immediate_calls: std::sync::atomic::AtomicUsize,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
struct SignalBeforeTransaction {
    inner: Arc<dyn StorageBackend>,
    entered: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl StorageBackend for SignalBeforeTransaction {
    async fn session(&self) -> note_storage::StorageResult<Box<dyn note_storage::StorageSession>> {
        self.inner.session().await
    }
    async fn begin(
        &self,
        mode: TransactionMode,
    ) -> note_storage::StorageResult<Box<dyn note_storage::StorageTransaction>> {
        self.entered.notify_one();
        self.inner.begin(mode).await
    }
    async fn info(&self) -> note_storage::StorageResult<note_storage::BackendInfo> {
        self.inner.info().await
    }
}
#[async_trait::async_trait]
impl StorageBackend for PauseBeforeBootstrapPage {
    async fn session(&self) -> note_storage::StorageResult<Box<dyn note_storage::StorageSession>> {
        self.inner.session().await
    }
    async fn begin(
        &self,
        mode: TransactionMode,
    ) -> note_storage::StorageResult<Box<dyn note_storage::StorageTransaction>> {
        if mode == TransactionMode::Immediate
            && self
                .immediate_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 2
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.begin(mode).await
    }
    async fn info(&self) -> note_storage::StorageResult<note_storage::BackendInfo> {
        self.inner.info().await
    }
}
#[tokio::test]
async fn paginated_bootstrap_interleaving_and_historical_rescan_preserve_newer_pending() {
    let (original, backend, dir) = support::test_context().await;
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    tx.insert_label_key_if_missing("project", "").await.unwrap();
    let ids: Vec<_> = (0..130)
        .map(|i| format!("00000000-0000-4000-8000-{i:012}"))
        .collect();
    for id in &ids {
        tx.insert_note(NewNote {
            id,
            title: "Historic",
            content: "before bootstrap",
            attachments: &[],
            created_at: 90,
            updated_at: 100,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
        tx.attach_label(id, "project", "bear_note").await.unwrap();
    }
    tx.set_system_config(&config()).await.unwrap();
    tx.commit().await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let wrapped: Arc<dyn StorageBackend> = Arc::new(PauseBeforeBootstrapPage {
        inner: backend.clone(),
        immediate_calls: std::sync::atomic::AtomicUsize::new(0),
        entered: entered.clone(),
        release: release.clone(),
    });
    let scanning = Arc::new(Context::new(
        wrapped,
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(::note_attachments::FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    ));
    let task = tokio::spawn({
        let scanning = scanning.clone();
        async move { initialize_sync(&scanning).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let first_page_status = sync_status(&original).await.unwrap();
    assert!(!first_page_status.initialized);
    assert_eq!(first_page_status.pending_notes, 128);
    let mut captured = Vec::new();
    for index in [0, 129] {
        let id = &ids[index];
        update_note_fields(
            &original,
            id,
            UpdateNoteFieldsInput {
                expected_revision: 1,
                title: "Updated during bootstrap".into(),
                content: format!("fresh {index}"),
                labels: vec![("project".into(), "bear_note".into())],
            },
        )
        .await
        .unwrap();
        captured.push((
            id.clone(),
            backend
                .session()
                .await
                .unwrap()
                .get_sync_record("pending", id)
                .await
                .unwrap(),
        ));
    }
    release.notify_one();
    task.await.unwrap().unwrap();
    assert!(sync_status(&original).await.unwrap().initialized);
    assert_eq!(sync_status(&original).await.unwrap().pending_notes, 130);
    for (id, value) in &captured {
        assert_eq!(
            &backend
                .session()
                .await
                .unwrap()
                .get_sync_record("pending", id)
                .await
                .unwrap(),
            value
        );
    }
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    let mut identity = tx
        .get_sync_record("identity", "local")
        .await
        .unwrap()
        .unwrap();
    identity["initialized"] = false.into();
    identity["cursor"] = serde_json::Value::Null;
    tx.put_sync_record("identity", "local", &identity)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    initialize_sync(&original).await.unwrap();
    for (id, value) in captured {
        assert_eq!(
            backend
                .session()
                .await
                .unwrap()
                .get_sync_record("pending", &id)
                .await
                .unwrap(),
            value
        );
    }
}
#[tokio::test]
async fn storage_reopen_preserves_identity_clock_pending_and_purge_tombstone() {
    let (ctx, backend, dir) = support::test_context().await;
    enabled(&ctx).await;
    let first = save_note(&ctx, input("persisted pending")).await.unwrap();
    let second = save_note(&ctx, input("persisted purge")).await.unwrap();
    delete_note(&ctx, &second.id, second.revision)
        .await
        .unwrap();
    let deleted = backend
        .session()
        .await
        .unwrap()
        .get_sync_note(&second.id)
        .await
        .unwrap()
        .unwrap();
    permanently_delete_note(&ctx, &second.id, deleted.revision)
        .await
        .unwrap();
    let s = backend.session().await.unwrap();
    let identity = s.get_sync_record("identity", "local").await.unwrap();
    let pending_first = s.get_sync_record("pending", &first.id).await.unwrap();
    let tombstone = s.get_sync_record("head", &second.id).await.unwrap();
    let pending_purge = s.get_sync_record("pending", &second.id).await.unwrap();
    drop(s);
    drop(ctx);
    drop(backend);
    let reopened: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let newctx = Context::new(
        reopened.clone(),
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(::note_attachments::FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    );
    initialize_sync(&newctx).await.unwrap();
    let s = reopened.session().await.unwrap();
    assert_eq!(
        s.get_sync_record("identity", "local").await.unwrap(),
        identity
    );
    assert_eq!(
        s.get_sync_record("pending", &first.id).await.unwrap(),
        pending_first
    );
    assert_eq!(
        s.get_sync_record("head", &second.id).await.unwrap(),
        tombstone
    );
    assert_eq!(
        s.get_sync_record("pending", &second.id).await.unwrap(),
        pending_purge
    );
    assert!(s.get_sync_note(&second.id).await.unwrap().is_none());
    update_note_fields(
        &newctx,
        &first.id,
        UpdateNoteFieldsInput {
            expected_revision: first.revision,
            title: first.title,
            content: "edit after restart".into(),
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap();
    let clock = s
        .get_sync_record("identity", "local")
        .await
        .unwrap()
        .unwrap()["clock"]
        .clone();
    let before: Hlc = serde_json::from_value(identity.unwrap()["clock"].clone()).unwrap();
    let after: Hlc = serde_json::from_value(clock).unwrap();
    assert!(after > before);
}
async fn records(backend: &Arc<dyn StorageBackend>) -> serde_json::Value {
    let session = backend.session().await.unwrap();
    let mut map = serde_json::Map::new();
    for ns in [
        "identity",
        "head",
        "member",
        "pending",
        "inflight",
        "deleting",
        "ack",
        "received",
        "scheduler",
    ] {
        map.insert(
            ns.into(),
            serde_json::to_value(session.list_sync_records(ns, None, 1000).await.unwrap()).unwrap(),
        );
    }
    map.into()
}
#[tokio::test]
async fn inflight_ack_requires_every_field_and_retains_pending_and_read_lease_on_mismatch() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let mut seed = input("exact ACK required");
    seed.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: "".into(),
        content: b"retained bytes".to_vec(),
        storage: None,
    });
    save_note(&ctx, seed).await.unwrap();
    let event = next_sync_event(&ctx).await.unwrap().unwrap();
    let before = records(&backend).await;
    for field in 0..6 {
        let mut ack = event.acknowledgement();
        match field {
            0 => ack.note_id = uuid::Uuid::new_v4().to_string(),
            1 => ack.generation.counter += 1,
            2 => ack.control.counter += 1,
            3 => ack.version.counter += 1,
            4 => ack.kind = SyncEventKind::Snapshot,
            _ => ack.snapshot_hash = "0".repeat(64),
        }
        let error = acknowledge_sync_event(&ctx, &ack).await.unwrap_err();
        assert!(error.to_string().contains("durable inflight event"));
        assert_eq!(records(&backend).await, before);
        assert_eq!(
            read_sync_attachment(&ctx, &event, "file", 1024)
                .await
                .unwrap(),
            b"retained bytes"
        );
    }
    let mut unknown = event.acknowledgement();
    unknown.event_id = uuid::Uuid::new_v4().to_string();
    acknowledge_sync_event(&ctx, &unknown).await.unwrap();
    assert_eq!(records(&backend).await, before);
    let exact = event.acknowledgement();
    acknowledge_sync_event(&ctx, &exact).await.unwrap();
    assert_no_pending(&ctx).await;
    let after = records(&backend).await;
    acknowledge_sync_event(&ctx, &exact).await.unwrap();
    assert_eq!(records(&backend).await, after);
    assert!(read_sync_attachment(&ctx, &event, "file", 1024)
        .await
        .is_err());
}
#[tokio::test]
async fn sync_capture_failure_rolls_back_business_note_clock_pending_and_pins() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let mut seed = input("before failure");
    seed.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: "".into(),
        content: b"before".to_vec(),
        storage: None,
    });
    let note = save_note(&ctx, seed).await.unwrap();
    next_sync_event(&ctx).await.unwrap().unwrap();
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    tx.put_sync_record(
        "member",
        &note.id,
        &serde_json::json!({"forced_invalid_member":true}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = records(&backend).await;
    let before_note = get_note_metadata(&ctx, &note.id).await.unwrap();
    let before_operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap();
    let mut changed = input("business edit must roll back");
    changed.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: "".into(),
        content: b"after".to_vec(),
        storage: None,
    });
    let error = update_note(&ctx, &note.id, note.revision, changed)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing field"));
    assert_eq!(records(&backend).await, before);
    assert_eq!(
        get_note_metadata(&ctx, &note.id).await.unwrap(),
        before_note
    );
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_attachment_operations_for_note(&note.id)
            .await
            .unwrap(),
        before_operations
    );
}
#[tokio::test]
async fn cleanup_reservation_and_reference_or_read_lease_obey_controlled_transaction_order() {
    let (ctx, backend, _dir) = support::test_context().await;
    enabled(&ctx).await;
    let mut seed = input("one");
    seed.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: "".into(),
        content: b"original".to_vec(),
        storage: None,
    });
    let note = save_note(&ctx, seed).await.unwrap();
    let leased = next_sync_event(&ctx).await.unwrap().unwrap();
    let leased_member = backend
        .session()
        .await
        .unwrap()
        .get_sync_record("member", &note.id)
        .await
        .unwrap()
        .unwrap();
    let old_storage = note.attachments[0].storage.as_ref().unwrap();
    let old_key = old_storage.object_key.clone();
    let mut changed = input("two");
    changed.attachments.push(NoteAttachment {
        id: "file".into(),
        path: "file.txt".into(),
        mime: "text/plain".into(),
        description: "".into(),
        content: b"replacement".to_vec(),
        storage: None,
    });
    update_note(&ctx, &note.id, note.revision, changed)
        .await
        .unwrap();
    assert!(
        !reserve_sync_attachment_cleanup(&ctx, &note.id, &old_key, false)
            .await
            .unwrap()
    );
    let mut paused = config();
    paused.sync.enabled = false;
    update_system_config(&ctx, &paused).await.unwrap();
    assert!(
        !reserve_sync_attachment_cleanup(&ctx, &note.id, &old_key, false)
            .await
            .unwrap()
    );
    update_system_config(&ctx, &config()).await.unwrap();
    release_sync_event(&ctx, &leased.event_id).await.unwrap();
    // Hold the reference transaction while cleanup really competes for its write lock.
    let reference_id = uuid::Uuid::new_v4().to_string();
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    let mut reference = note.attachments[0].clone();
    reference.content.clear();
    tx.insert_note(NewNote {
        id: &reference_id,
        title: "Reference",
        content: "old immutable object",
        attachments: &[reference],
        created_at: 1,
        updated_at: 1,
        note_revision: 1,
        deleted_at: None,
    })
    .await
    .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let competing = Context::new(
        Arc::new(SignalBeforeTransaction {
            inner: backend.clone(),
            entered: entered.clone(),
        }),
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(::note_attachments::FilesystemAttachmentStore::new(
            _dir.path().join("attachments"),
        )),
    );
    let mut cleanup = Box::pin(reserve_sync_attachment_cleanup(
        &competing, &note.id, &old_key, false,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = &mut cleanup => panic!("cleanup completed before reference commit: {result:?}"),
            _ = entered.notified() => {}
        }
    }).await.unwrap();
    assert!(futures_util::poll!(cleanup.as_mut()).is_pending());
    tx.commit().await.unwrap();
    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(5), cleanup)
            .await
            .unwrap()
            .unwrap()
    );
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    tx.purge_sync_note(&reference_id).await.unwrap();
    tx.commit().await.unwrap();
    // Reservation first: even a current inventory event cannot acquire a read lease.
    assert!(
        reserve_sync_attachment_cleanup(&ctx, &note.id, &old_key, false)
            .await
            .unwrap()
    );
    let tx = backend.begin(TransactionMode::Immediate).await.unwrap();
    tx.put_sync_record("member", &note.id, &leased_member)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let current_member = backend
        .session()
        .await
        .unwrap()
        .get_sync_record("member", &note.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_value::<SyncEvent>(current_member["event"].clone()).unwrap(),
        leased
    );
    let error = reserve_sync_event(&ctx, &leased).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("attachment cleanup has reserved object"));
    assert!(read_sync_attachment(&ctx, &leased, "file", 100)
        .await
        .is_err());
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_sync_record("inflight", &leased.event_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn enabled_import_bulk_catalog_and_attachment_mutations_are_captured() {
    let (ctx, _backend, _dir) = support::test_context().await;
    mutation_family_contracts(&ctx).await;
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL live PostgreSQL"]
async fn postgres_mutation_families_capture_snapshots() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let admin = ::sqlx::PgPool::connect(&url).await.unwrap();
    // Generated UUID hex ensures this identifier cannot contain SQL syntax.
    let schema = format!("sync_{}", uuid::Uuid::new_v4().simple());
    ::sqlx::query(::sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut isolated_url = url::Url::parse(&url).unwrap();
    isolated_url
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema},public"));
    let concrete = Arc::new(
        note_storage_pg::PgStorage::connect(isolated_url.as_str(), 5)
            .await
            .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::new(
        concrete.clone(),
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(::note_attachments::FilesystemAttachmentStore::new(
            dir.path().to_path_buf(),
        )),
    );
    mutation_family_contracts(&ctx).await;
    drop(ctx);
    concrete.close().await;
    ::sqlx::query(::sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA {schema} CASCADE"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
}
