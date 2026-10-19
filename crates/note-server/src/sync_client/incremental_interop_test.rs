//! Opt-in qualification against the real GaoNote implementation and an isolated DB.
use super::*;

async fn control(client: &reqwest::Client, url: &str, payload: Value) -> Value {
    client
        .post(url)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn peer<'a>(ctx: &'a Context, runtime: &'a SyncRuntime, url: &str) -> Session<'a> {
    sync::initialize_sync(ctx).await.unwrap();
    let profile = SyncConnection {
        id: "test".into(),
        url: url.into(),
        mode: ConnectionMode::Direct,
        ssh: None,
        client_cert: PathBuf::new(),
        client_key: PathBuf::new(),
        ca_cert: None,
        max_blob_bytes: super::super::DEFAULT_MAX_BLOB_BYTES,
    };
    let mut session = Session {
        ctx,
        runtime,
        socket: connect_socket(&profile).await.unwrap(),
        join: uuid::Uuid::new_v4().to_string(),
        counter: 0,
        limit: profile.max_blob_bytes,
        incoming: None,
        heartbeat: None,
        next_heartbeat: Instant::now() + HEARTBEAT,
        outbound: None,
        state_query: false,
        inbound_query: None,
        remote_inventory_complete: false,
    };
    let hello = Hello {
        protocol_version: 1,
        node_id: sync::sync_status(ctx).await.unwrap().node_id,
        selectors: vec!["project=bear_note".into()],
        max_blob_bytes: profile.max_blob_bytes,
    };
    let response: PeerHello = serde_json::from_value(
        session
            .request("phx_join", serde_json::to_value(&hello).unwrap())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response.hello.selectors, hello.selectors);
    assert_ne!(response.hello.node_id, hello.node_id);
    assert!(response.capabilities.iter().any(|c| c == "state_query_v1"));
    sync::pin_sync_peer(ctx, &profile.id, &response.hello.node_id)
        .await
        .unwrap();
    session.limit = profile.max_blob_bytes.min(response.hello.max_blob_bytes);
    let selection = json!({"capabilities":["state_query_v1"]});
    assert_eq!(
        session
            .request("sync_negotiate", selection.clone())
            .await
            .unwrap(),
        selection
    );
    session.state_query = true;
    runtime
        .progress(|p| *p = SyncProgress::new("remote_verified"))
        .await;
    session
}

async fn finish_remote_inventory(session: &mut Session<'_>) {
    timeout(Duration::from_secs(30), async {
        while !session.remote_inventory_complete {
            session.check_liveness().await.unwrap();
            let wake = session.next_wake();
            tokio::select! {
                message = session.socket.next() => session.handle_message(message).await.unwrap(),
                _ = tokio::time::sleep_until(wake) => {},
            }
        }
    })
    .await
    .expect("GaoNote inventory did not finish");
}

#[tokio::test]
#[ignore = "requires isolated GaoNote listener; GAONOTE_INTEROP_WS and GAONOTE_INTEROP_HTTP"]
async fn real_gaonote_reconnect_skips_blobs_transfers_changes_and_restores_membership() {
    let ws = std::env::var("GAONOTE_INTEROP_WS").unwrap();
    let http = std::env::var("GAONOTE_INTEROP_HTTP").unwrap();
    for address in [&ws, &http] {
        let parsed = reqwest::Url::parse(address).unwrap();
        assert_eq!(
            parsed.host_str(),
            Some("127.0.0.1"),
            "use the isolated loopback fixture only"
        );
    }
    let http_client = reqwest::Client::new();
    let seed = control(
        &http_client,
        &http,
        json!({"op":"create_note","title":"GaoNote interop seed","content":"Remote seed"}),
    )
    .await;
    let seed_id = seed["note_id"].as_str().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::new(
        Arc::new(
            note_storage_turso::TursoStorage::open(dir.path().join("db"))
                .await
                .unwrap(),
        ),
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(note_attachments::FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    );
    note_pipelines::update_system_config(
        &ctx,
        &note_core::SystemConfig {
            sync: note_core::sync::SyncConfig {
                enabled: true,
                connection_profile: Some("test".into()),
                rules: vec![note_core::sync::SyncRule {
                    id: "test".into(),
                    enabled: true,
                    selector: "project=bear_note".into(),
                }],
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let local = note_pipelines::save_note(
        &ctx,
        note_pipelines::SaveNoteInput {
            title: "Agent Note interop file".into(),
            content: "Initial body".into(),
            labels: vec![("project".into(), "bear_note".into())],
            attachments: vec![note_core::NoteAttachment {
                id: "event".into(),
                path: "file.bin".into(),
                mime: "application/octet-stream".into(),
                description: String::new(),
                content: vec![42; CHUNK + 13],
                storage: None,
            }],
        },
    )
    .await
    .unwrap();
    let runtime = SyncRuntime::default();
    let mut connection = peer(&ctx, &runtime, &ws).await;
    connection
        .request("inventory_start", json!({}))
        .await
        .unwrap();
    connection.reconcile_round(true).await.unwrap();
    finish_remote_inventory(&mut connection).await;
    assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 0);
    assert_eq!(
        note_pipelines::get_note_metadata(&ctx, seed_id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "Remote seed"
    );
    let received = control(
        &http_client,
        &http,
        json!({"op":"get_note","note_id":local.id}),
    )
    .await;
    assert_eq!(received["note"]["content"], "Initial body");
    drop(connection);

    control(&http_client, &http, json!({"op":"reset_storage_requests"})).await;
    let mut connection = peer(&ctx, &runtime, &ws).await;
    connection
        .request("inventory_start", json!({}))
        .await
        .unwrap();
    connection.reconcile_round(true).await.unwrap();
    finish_remote_inventory(&mut connection).await;
    {
        let status = runtime.status.read().await;
        let upload = &status.progress.as_ref().unwrap().upload;
        assert_eq!(upload.notes_remaining, Some(0));
        assert_eq!(
            upload.notes_equal,
            upload.notes_total.unwrap(),
            "{upload:?}"
        );
        assert_eq!(upload.notes_completed, 0);
    }
    let requests = control(&http_client, &http, json!({"op":"storage_requests"})).await;
    assert!(
        requests["requests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|request| request["method"] == "HEAD"
                || (request["method"] == "GET"
                    && request["key"]
                        .as_str()
                        .unwrap()
                        .starts_with(".gsmlg-storage/objects/"))),
        "reconnect accessed blob bytes: {requests}"
    );

    let note = note_pipelines::get_note(&ctx, &local.id)
        .await
        .unwrap()
        .unwrap();
    note_pipelines::update_note(
        &ctx,
        &note.id,
        note.revision,
        note_pipelines::SaveNoteInput {
            title: note.title,
            content: "Changed on the same socket".into(),
            labels: vec![("project".into(), "bear_note".into())],
            attachments: note.attachments,
        },
    )
    .await
    .unwrap();
    let same_join = connection.join.clone();
    connection.reconcile_round(false).await.unwrap();
    assert_eq!(connection.join, same_join);
    assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 0);
    assert_eq!(
        runtime
            .status
            .read()
            .await
            .progress
            .as_ref()
            .unwrap()
            .upload
            .notes_completed,
        1
    );
    let received = control(
        &http_client,
        &http,
        json!({"op":"get_note","note_id":local.id}),
    )
    .await;
    assert_eq!(received["note"]["content"], "Changed on the same socket");
    drop(connection);

    // No inventory is running on this new session, so its sender cannot recreate
    // the missing membership before the explicit recovery probe.
    let mut connection = peer(&ctx, &runtime, &ws).await;
    let node_id = sync::sync_status(&ctx).await.unwrap().node_id;
    control(
        &http_client,
        &http,
        json!({"op":"drop_membership","note_id":local.id,"peer_id":node_id}),
    )
    .await;
    let before = sync::sync_current_candidate(&ctx, &local.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!before.pending);
    let missing = connection
        .query_state(vec![before.descriptor.clone()])
        .await
        .unwrap();
    assert_eq!(missing.items[0].association, SyncAssociation::Missing);
    assert!(
        sync::recover_missing_sync_association(&ctx, &before.descriptor)
            .await
            .unwrap()
    );
    let recovered = sync::sync_current_candidate(&ctx, &local.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.descriptor.kind, SyncEventKind::Attach);
    assert_eq!(
        recovered.descriptor.generation,
        before.descriptor.generation
    );
    assert_eq!(recovered.descriptor.version, before.descriptor.version);
    connection.deliver_candidate(recovered).await.unwrap();
    let after = sync::sync_current_candidate(&ctx, &local.id)
        .await
        .unwrap()
        .unwrap();
    let restored = connection
        .query_state(vec![after.descriptor.clone()])
        .await
        .unwrap();
    assert!(restored.items[0].matches_snapshot(&after.descriptor));
    assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 0);
    eprintln!("GaoNote interop PASS: duplex inventory, reconnect no blobs, one changed note on same socket, missing membership restored");
}
