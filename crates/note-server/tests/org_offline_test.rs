use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::org::{
    archive_document, archive_workspace, claim_item, create_workspace, put_document,
    ArchiveWorkspaceRequest, CommandEnvelope, CreateWorkspaceRequest, DocumentRevisionRequest,
    FixedOrgClock, OrgClaimKind, OrgContext, PutDocumentRequest, StartClaimRequest,
};
use note_pipelines::Context;
use note_server::org_offline::{
    execute_command, parse_command, ImportMode, OrgOfflineCommand, WorkspaceManifest,
};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt as _;

const NOW: i64 = 1_800_000_000;
const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000001";
const DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000001";
const SECOND_DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000002";
const ITEM_ID: &str = "30000000-0000-4000-8000-000000000001";
const SECOND_ITEM_ID: &str = "30000000-0000-4000-8000-000000000002";

fn args(values: &[&str]) -> Vec<String> {
    std::iter::once("note-server".to_owned())
        .chain(values.iter().map(|value| (*value).to_owned()))
        .collect()
}

async fn test_context(path: &Path) -> (OrgContext, Arc<dyn StorageBackend>) {
    let storage: Arc<dyn StorageBackend> = Arc::new(TursoStorage::open(path).await.unwrap());
    (
        OrgContext::new(storage.clone(), Arc::new(FixedOrgClock::new(NOW))),
        storage,
    )
}

fn envelope(operation_id: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id: WORKSPACE_ID.parse().unwrap(),
        actor_id: "offline-test".into(),
        operation_id: operation_id.into(),
    }
}

fn source(item_id: &str, line_end: &str, body: &str) -> String {
    format!(
        "#+TITLE: Exact bytes{line_end}#+CUSTOM: opaque syntax{line_end}* READY Ship offline{line_end}:PROPERTIES:{line_end}:ID: {item_id}{line_end}:AGENT_NOTE_TYPE: task{line_end}:END:{line_end}{body}{line_end}"
    )
}

async fn seed_workspace(context: &OrgContext) -> (String, String) {
    create_workspace(
        context,
        &envelope("seed-workspace"),
        &CreateWorkspaceRequest {
            slug: "offline".into(),
            display_name: "Offline workspace".into(),
            description: "Portable snapshot".into(),
            timezone: "Asia/Shanghai".into(),
            policy_schema_version: 1,
            policy: note_org::WorkspacePolicy::engineering_default(),
        },
    )
    .await
    .unwrap();
    let first = source(ITEM_ID, "\r\n", "Opaque  CRLF");
    let second = source(SECOND_ITEM_ID, "\n", "Opaque\tLF");
    for (operation, document_id, path, raw) in [
        ("seed-first", DOCUMENT_ID, "inbox/tasks.org", &first),
        ("seed-second", SECOND_DOCUMENT_ID, "plans/next.org", &second),
    ] {
        put_document(
            context,
            &envelope(operation),
            &PutDocumentRequest {
                document_id: document_id.parse().unwrap(),
                path: path.into(),
                source: raw.clone(),
                expected_revision: None,
                lease_proofs: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    }
    (first, second)
}

#[tokio::test]
async fn workspace_manifest_round_trips_document_archive_state_and_accepts_legacy_omission() {
    let dir = tempfile::tempdir().unwrap();
    let (source_context, _source_storage) = test_context(&dir.path().join("source.db")).await;
    let (first, second) = seed_workspace(&source_context).await;
    archive_document(
        &source_context,
        &envelope("archive-offline-document"),
        &DocumentRevisionRequest {
            document_id: SECOND_DOCUMENT_ID.parse().unwrap(),
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    let snapshot = dir.path().join("lifecycle-snapshot");
    let exported = execute_command(
        &source_context,
        OrgOfflineCommand::ExportWorkspace {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            output: snapshot.clone(),
        },
    )
    .await;
    assert!(exported.ok, "{exported:?}");
    let manifest = read_manifest(&snapshot);
    let active = manifest
        .documents
        .iter()
        .find(|document| document.id.to_string() == DOCUMENT_ID)
        .unwrap();
    let archived = manifest
        .documents
        .iter()
        .find(|document| document.id.to_string() == SECOND_DOCUMENT_ID)
        .unwrap();
    assert_eq!(active.archived_at, None);
    assert_eq!(archived.archived_at, Some(NOW));
    assert_eq!(archived.revision, 2);

    let (target_context, target_storage) = test_context(&dir.path().join("target.db")).await;
    let imported = execute_command(
        &target_context,
        OrgOfflineCommand::ImportWorkspace {
            input: snapshot.clone(),
            mode: ImportMode::Create,
            actor_id: "offline-restore".into(),
            operation_id: "restore-document-lifecycle".into(),
        },
    )
    .await;
    assert!(imported.ok, "{imported:?}");
    let session = target_storage.session().await.unwrap();
    let active_record = session
        .get_org_document(DOCUMENT_ID.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let archived_record = session
        .get_org_document(SECOND_DOCUMENT_ID.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active_record.source, first);
    assert_eq!(active_record.archived_at, None);
    assert_eq!(archived_record.source, second);
    assert_eq!(archived_record.archived_at, Some(NOW));
    assert_eq!(archived_record.revision, 2);
    drop(session);

    let legacy = dir.path().join("legacy-snapshot");
    copy_snapshot(&snapshot, &legacy);
    let mut legacy_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(legacy.join("manifest.json")).unwrap()).unwrap();
    for document in legacy_manifest["documents"].as_array_mut().unwrap() {
        document.as_object_mut().unwrap().remove("archived_at");
    }
    std::fs::write(
        legacy.join("manifest.json"),
        serde_json::to_vec_pretty(&legacy_manifest).unwrap(),
    )
    .unwrap();
    let (legacy_context, legacy_storage) = test_context(&dir.path().join("legacy.db")).await;
    let legacy_import = execute_command(
        &legacy_context,
        OrgOfflineCommand::ImportWorkspace {
            input: legacy,
            mode: ImportMode::Create,
            actor_id: "legacy-restore".into(),
            operation_id: "restore-legacy-manifest".into(),
        },
    )
    .await;
    assert!(legacy_import.ok, "{legacy_import:?}");
    assert_eq!(
        legacy_storage
            .session()
            .await
            .unwrap()
            .get_org_document(SECOND_DOCUMENT_ID.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .archived_at,
        None
    );

    let invalid = dir.path().join("invalid-lifecycle-snapshot");
    copy_snapshot(&snapshot, &invalid);
    let mut invalid_manifest = read_manifest(&invalid);
    invalid_manifest.documents[0].archived_at = Some(0);
    write_manifest(&invalid, &invalid_manifest);
    let (invalid_context, invalid_storage) = test_context(&dir.path().join("invalid.db")).await;
    let invalid_report = execute_command(
        &invalid_context,
        OrgOfflineCommand::ImportWorkspace {
            input: invalid,
            mode: ImportMode::Create,
            actor_id: "invalid-restore".into(),
            operation_id: "reject-invalid-document-timestamp".into(),
        },
    )
    .await;
    assert!(!invalid_report.ok);
    assert_eq!(invalid_report.error.as_ref().unwrap().code, "invalid_input");
    let rendered = serde_json::to_string(&invalid_report).unwrap();
    assert!(!rendered.contains("source"));
    assert!(!rendered.contains("fencing_token"));
    assert!(invalid_storage
        .session()
        .await
        .unwrap()
        .get_org_workspace(WORKSPACE_ID.parse().unwrap())
        .await
        .unwrap()
        .is_none());
}

fn read_manifest(path: &Path) -> WorkspaceManifest {
    serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap()
}

fn write_manifest(path: &Path, manifest: &WorkspaceManifest) {
    std::fs::write(
        path.join("manifest.json"),
        serde_json::to_vec_pretty(manifest).unwrap(),
    )
    .unwrap();
}

fn copy_snapshot(source: &Path, target: &Path) {
    std::fs::create_dir(target).unwrap();
    std::fs::create_dir(target.join("documents")).unwrap();
    std::fs::copy(source.join("manifest.json"), target.join("manifest.json")).unwrap();
    for entry in std::fs::read_dir(source.join("documents")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(
            entry.path(),
            target.join("documents").join(entry.file_name()),
        )
        .unwrap();
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

async fn call_org_mcp(
    router: &Router,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "offline-acceptance.example.test")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": name, "arguments": arguments}
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[test]
fn parses_the_four_exact_org_offline_modes() {
    assert_eq!(
        parse_command(&args(&[
            "org",
            "export-workspace",
            "--workspace-id",
            "10000000-0000-4000-8000-000000000001",
            "--output",
            "snapshot",
        ]))
        .unwrap(),
        Some(OrgOfflineCommand::ExportWorkspace {
            workspace_id: "10000000-0000-4000-8000-000000000001".parse().unwrap(),
            output: PathBuf::from("snapshot"),
        })
    );
    assert_eq!(
        parse_command(&args(&[
            "org",
            "import-workspace",
            "--input",
            "snapshot",
            "--mode",
            "update",
            "--actor-id",
            "operator",
            "--operation-id",
            "restore-1",
        ]))
        .unwrap(),
        Some(OrgOfflineCommand::ImportWorkspace {
            input: PathBuf::from("snapshot"),
            mode: ImportMode::Update,
            actor_id: "operator".to_owned(),
            operation_id: "restore-1".to_owned(),
        })
    );
    assert_eq!(
        parse_command(&args(&[
            "org",
            "export-document",
            "--document-id",
            "20000000-0000-4000-8000-000000000001",
            "--output",
            "document.org",
        ]))
        .unwrap(),
        Some(OrgOfflineCommand::ExportDocument {
            document_id: "20000000-0000-4000-8000-000000000001".parse().unwrap(),
            output: PathBuf::from("document.org"),
        })
    );
    assert_eq!(
        parse_command(&args(&[
            "org",
            "import-document",
            "--workspace-id",
            "10000000-0000-4000-8000-000000000001",
            "--document-id",
            "20000000-0000-4000-8000-000000000001",
            "--path",
            "inbox/tasks.org",
            "--input",
            "document.org",
            "--mode",
            "update",
            "--actor-id",
            "operator",
            "--operation-id",
            "document-1",
            "--expected-revision",
            "7",
        ]))
        .unwrap(),
        Some(OrgOfflineCommand::ImportDocument {
            workspace_id: "10000000-0000-4000-8000-000000000001".parse().unwrap(),
            document_id: "20000000-0000-4000-8000-000000000001".parse().unwrap(),
            path: "inbox/tasks.org".to_owned(),
            input: PathBuf::from("document.org"),
            mode: ImportMode::Update,
            actor_id: "operator".to_owned(),
            operation_id: "document-1".to_owned(),
            expected_revision: Some(7),
        })
    );
}

#[test]
fn rejects_missing_duplicate_unknown_and_incompatible_arguments() {
    for invalid in [
        args(&["org"]),
        args(&["org", "unknown"]),
        args(&["org", "export-workspace", "--workspace-id", "id"]),
        args(&[
            "org",
            "export-document",
            "--document-id",
            "id",
            "--document-id",
            "other",
            "--output",
            "out.org",
        ]),
        args(&[
            "org",
            "import-workspace",
            "--input",
            "snapshot",
            "--mode",
            "replace",
            "--actor-id",
            "operator",
            "--operation-id",
            "op",
        ]),
        args(&[
            "org",
            "import-document",
            "--workspace-id",
            "workspace",
            "--document-id",
            "document",
            "--path",
            "tasks.org",
            "--input",
            "document.org",
            "--mode",
            "create",
            "--actor-id",
            "operator",
            "--operation-id",
            "op",
            "--expected-revision",
            "1",
        ]),
        args(&[
            "org",
            "import-document",
            "--workspace-id",
            "workspace",
            "--document-id",
            "document",
            "--path",
            "tasks.org",
            "--input",
            "document.org",
            "--mode",
            "update",
            "--actor-id",
            "operator",
            "--operation-id",
            "op",
        ]),
    ] {
        assert!(parse_command(&invalid).is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn preserves_legacy_non_org_modes() {
    for legacy in [
        args(&["--import"]),
        args(&["--export"]),
        args(&["--stdio"]),
        args(&[]),
    ] {
        assert_eq!(parse_command(&legacy).unwrap(), None);
    }
}

#[tokio::test]
async fn workspace_and_document_exports_are_deterministic_byte_exact_and_refuse_nonempty_targets() {
    let dir = tempfile::tempdir().unwrap();
    let (context, _storage) = test_context(&dir.path().join("source.db")).await;
    let (first, second) = seed_workspace(&context).await;
    let snapshot = dir.path().join("snapshot");

    let exported = execute_command(
        &context,
        OrgOfflineCommand::ExportWorkspace {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            output: snapshot.clone(),
        },
    )
    .await;
    assert!(exported.ok, "{exported:?}");
    let manifest = read_manifest(&snapshot);
    assert_eq!(manifest.format_version, 1);
    assert_eq!(manifest.workspace.id.to_string(), WORKSPACE_ID);
    assert_eq!(manifest.workspace.revision, 1);
    assert_eq!(
        manifest
            .documents
            .iter()
            .map(|document| document.id.to_string())
            .collect::<Vec<_>>(),
        vec![DOCUMENT_ID, SECOND_DOCUMENT_ID]
    );
    for (entry, expected) in manifest.documents.iter().zip([&first, &second]) {
        let bytes = std::fs::read(snapshot.join(&entry.file)).unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(entry.content_hash, hash(&bytes));
        assert_eq!(entry.revision, 1);
        assert_eq!(entry.file, format!("documents/{}.org", entry.id));
    }
    let before = std::fs::read(snapshot.join("manifest.json")).unwrap();
    let refused = execute_command(
        &context,
        OrgOfflineCommand::ExportWorkspace {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            output: snapshot.clone(),
        },
    )
    .await;
    assert!(!refused.ok);
    assert_eq!(
        std::fs::read(snapshot.join("manifest.json")).unwrap(),
        before
    );

    let document_output = dir.path().join("single.org");
    let document_export = execute_command(
        &context,
        OrgOfflineCommand::ExportDocument {
            document_id: DOCUMENT_ID.parse().unwrap(),
            output: document_output.clone(),
        },
    )
    .await;
    assert!(document_export.ok);
    assert_eq!(std::fs::read(&document_output).unwrap(), first.as_bytes());

    let empty_workspace_target = dir.path().join("empty-workspace-target");
    std::fs::create_dir(&empty_workspace_target).unwrap();
    let empty_workspace_refused = execute_command(
        &context,
        OrgOfflineCommand::ExportWorkspace {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            output: empty_workspace_target.clone(),
        },
    )
    .await;
    assert!(!empty_workspace_refused.ok);
    assert!(std::fs::read_dir(&empty_workspace_target)
        .unwrap()
        .next()
        .is_none());

    let empty_document_target = dir.path().join("empty-document-target.org");
    std::fs::write(&empty_document_target, b"").unwrap();
    let empty_document_refused = execute_command(
        &context,
        OrgOfflineCommand::ExportDocument {
            document_id: DOCUMENT_ID.parse().unwrap(),
            output: empty_document_target.clone(),
        },
    )
    .await;
    assert!(!empty_document_refused.ok);
    assert_eq!(std::fs::read(&empty_document_target).unwrap(), b"");
    let refused = execute_command(
        &context,
        OrgOfflineCommand::ExportDocument {
            document_id: DOCUMENT_ID.parse().unwrap(),
            output: document_output.clone(),
        },
    )
    .await;
    assert!(!refused.ok);
    assert_eq!(std::fs::read(&document_output).unwrap(), first.as_bytes());

    let (restored, restored_storage) = test_context(&dir.path().join("restored.db")).await;
    let imported = execute_command(
        &restored,
        OrgOfflineCommand::ImportWorkspace {
            input: snapshot.clone(),
            mode: ImportMode::Create,
            actor_id: "restore-agent".into(),
            operation_id: "restore-create".into(),
        },
    )
    .await;
    assert!(imported.ok, "{imported:?}");
    let session = restored_storage.session().await.unwrap();
    let restored_workspace = session
        .get_org_workspace(WORKSPACE_ID.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored_workspace.revision, manifest.workspace.revision);
    for entry in &manifest.documents {
        let document = session.get_org_document(entry.id).await.unwrap().unwrap();
        assert_eq!(document.revision, entry.revision);
        assert_eq!(document.content_hash, entry.content_hash);
        assert_eq!(
            document.source.as_bytes(),
            std::fs::read(snapshot.join(&entry.file)).unwrap()
        );
    }

    archive_workspace(
        &context,
        &envelope("archive-for-export"),
        &ArchiveWorkspaceRequest {
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    let archived_snapshot = dir.path().join("archived-snapshot");
    assert!(
        execute_command(
            &context,
            OrgOfflineCommand::ExportWorkspace {
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                output: archived_snapshot.clone(),
            },
        )
        .await
        .ok
    );
    let archived_manifest = read_manifest(&archived_snapshot);
    assert_eq!(archived_manifest.workspace.revision, 2);
    assert_eq!(archived_manifest.workspace.archived_at, Some(NOW));
    let (archived_context, archived_storage) =
        test_context(&dir.path().join("archived-restored.db")).await;
    assert!(
        execute_command(
            &archived_context,
            OrgOfflineCommand::ImportWorkspace {
                input: archived_snapshot,
                mode: ImportMode::Create,
                actor_id: "restore-agent".into(),
                operation_id: "restore-archived".into(),
            },
        )
        .await
        .ok
    );
    let archived = archived_storage
        .session()
        .await
        .unwrap()
        .get_org_workspace(WORKSPACE_ID.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(archived.revision, 2);
    assert_eq!(archived.archived_at, Some(NOW));
}

async fn assert_snapshot_rejected_without_writes(snapshot: &Path, database: &Path) {
    let workspace_id = WORKSPACE_ID.parse().unwrap();
    let (context, storage) = test_context(database).await;
    let report = execute_command(
        &context,
        OrgOfflineCommand::ImportWorkspace {
            input: snapshot.to_owned(),
            mode: ImportMode::Create,
            actor_id: "security-test".into(),
            operation_id: format!("reject-{}", database.file_stem().unwrap().to_string_lossy()),
        },
    )
    .await;
    assert!(!report.ok, "accepted invalid snapshot at {snapshot:?}");
    assert!(!report.applied);
    assert!(storage
        .session()
        .await
        .unwrap()
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_import_rejects_manifest_and_filesystem_escape_or_corruption_before_writes() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let (context, _storage) = test_context(&dir.path().join("source.db")).await;
    seed_workspace(&context).await;
    let base = dir.path().join("base");
    assert!(
        execute_command(
            &context,
            OrgOfflineCommand::ExportWorkspace {
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                output: base.clone(),
            },
        )
        .await
        .ok
    );

    for (index, mutation) in ["version", "duplicate", "traversal", "absolute", "hash"]
        .into_iter()
        .enumerate()
    {
        let snapshot = dir.path().join(format!("invalid-{mutation}"));
        copy_snapshot(&base, &snapshot);
        let mut manifest = read_manifest(&snapshot);
        match mutation {
            "version" => manifest.format_version = 99,
            "duplicate" => manifest.documents.push(manifest.documents[0].clone()),
            "traversal" => manifest.documents[0].path = "../escape.org".into(),
            "absolute" => manifest.documents[0].path = "/escape.org".into(),
            "hash" => manifest.documents[0].content_hash = format!("sha256:{}", "0".repeat(64)),
            _ => unreachable!(),
        }
        write_manifest(&snapshot, &manifest);
        assert_snapshot_rejected_without_writes(
            &snapshot,
            &dir.path().join(format!("rejected-{index}.db")),
        )
        .await;
    }

    let missing = dir.path().join("missing");
    copy_snapshot(&base, &missing);
    let missing_manifest = read_manifest(&missing);
    std::fs::remove_file(missing.join(&missing_manifest.documents[0].file)).unwrap();
    assert_snapshot_rejected_without_writes(&missing, &dir.path().join("missing.db")).await;

    let extra = dir.path().join("extra");
    copy_snapshot(&base, &extra);
    std::fs::write(extra.join("documents/extra.org"), b"extra").unwrap();
    assert_snapshot_rejected_without_writes(&extra, &dir.path().join("extra.db")).await;

    let malformed = dir.path().join("malformed");
    copy_snapshot(&base, &malformed);
    std::fs::write(malformed.join("manifest.json"), b"{not json").unwrap();
    let (malformed_context, malformed_storage) =
        test_context(&dir.path().join("malformed.db")).await;
    let report = execute_command(
        &malformed_context,
        OrgOfflineCommand::ImportWorkspace {
            input: malformed,
            mode: ImportMode::Create,
            actor_id: "security-test".into(),
            operation_id: "malformed".into(),
        },
    )
    .await;
    assert!(!report.ok);
    assert!(malformed_storage
        .session()
        .await
        .unwrap()
        .list_org_workspaces(true)
        .await
        .unwrap()
        .is_empty());

    let invalid_utf8 = dir.path().join("invalid-utf8");
    copy_snapshot(&base, &invalid_utf8);
    let mut manifest = read_manifest(&invalid_utf8);
    let bytes = vec![0xff, 0xfe, 0xfd];
    std::fs::write(invalid_utf8.join(&manifest.documents[0].file), &bytes).unwrap();
    manifest.documents[0].content_hash = hash(&bytes);
    write_manifest(&invalid_utf8, &manifest);
    assert_snapshot_rejected_without_writes(&invalid_utf8, &dir.path().join("invalid-utf8.db"))
        .await;

    let linked = dir.path().join("linked");
    copy_snapshot(&base, &linked);
    let manifest = read_manifest(&linked);
    let document_path = linked.join(&manifest.documents[0].file);
    std::fs::remove_file(&document_path).unwrap();
    symlink(base.join(&manifest.documents[0].file), &document_path).unwrap();
    assert_snapshot_rejected_without_writes(&linked, &dir.path().join("linked.db")).await;

    let linked_ancestor = dir.path().join("linked-ancestor");
    symlink(&base, &linked_ancestor).unwrap();
    let (linked_context, linked_storage) =
        test_context(&dir.path().join("linked-ancestor.db")).await;
    let report = execute_command(
        &linked_context,
        OrgOfflineCommand::ImportWorkspace {
            input: linked_ancestor,
            mode: ImportMode::Create,
            actor_id: "security-test".into(),
            operation_id: "linked-ancestor".into(),
        },
    )
    .await;
    assert!(!report.ok);
    assert!(linked_storage
        .session()
        .await
        .unwrap()
        .list_org_workspaces(true)
        .await
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn export_rejects_a_symlinked_output_ancestor_without_external_side_effects() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let (context, _storage) = test_context(&dir.path().join("source.db")).await;
    seed_workspace(&context).await;
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let linked = dir.path().join("linked-output");
    symlink(&outside, &linked).unwrap();

    let reports = [
        execute_command(
            &context,
            OrgOfflineCommand::ExportWorkspace {
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                output: linked.join("workspace-parent/snapshot"),
            },
        )
        .await,
        execute_command(
            &context,
            OrgOfflineCommand::ExportDocument {
                document_id: DOCUMENT_ID.parse().unwrap(),
                output: linked.join("document-parent/document.org"),
            },
        )
        .await,
    ];

    assert!(reports.iter().all(|report| !report.ok));
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}

#[tokio::test]
async fn workspace_manifest_rejects_nested_unknown_policy_and_tag_rule_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (context, _storage) = test_context(&dir.path().join("source.db")).await;
    seed_workspace(&context).await;
    let base = dir.path().join("base");
    assert!(
        execute_command(
            &context,
            OrgOfflineCommand::ExportWorkspace {
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                output: base.clone(),
            },
        )
        .await
        .ok
    );

    for (index, mutate) in ["policy", "tag-rule"].into_iter().enumerate() {
        let snapshot = dir.path().join(mutate);
        copy_snapshot(&base, &snapshot);
        let manifest_path = snapshot.join("manifest.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        if mutate == "policy" {
            value["workspace"]["policy"]["unexpected_nested"] = serde_json::json!(true);
        } else {
            value["workspace"]["policy"]["tag_rules"]["task"] = serde_json::json!({
                "allowed": [],
                "required": [],
                "unexpected_nested": true
            });
        }
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert_snapshot_rejected_without_writes(
            &snapshot,
            &dir.path().join(format!("nested-unknown-{index}.db")),
        )
        .await;
    }
}

#[tokio::test]
async fn workspace_manifest_rejects_duplicate_outer_policy_and_tag_rule_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (context, _storage) = test_context(&dir.path().join("source.db")).await;
    seed_workspace(&context).await;
    let base = dir.path().join("base");
    assert!(
        execute_command(
            &context,
            OrgOfflineCommand::ExportWorkspace {
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                output: base.clone(),
            },
        )
        .await
        .ok
    );
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(base.join("manifest.json")).unwrap()).unwrap();
    let workspace = serde_json::to_string(&value["workspace"]).unwrap();
    let documents = serde_json::to_string(&value["documents"]).unwrap();
    let policy = serde_json::to_string(&value["workspace"]["policy"]).unwrap();
    let workspace_duplicate_revision =
        workspace.replacen("\"revision\":1", "\"revision\":1,\"revision\":1", 1);
    let workspace_duplicate_policy = workspace.replacen(
        &format!("\"policy\":{policy}"),
        &format!("\"policy\":{policy},\"policy\":{policy}"),
        1,
    );
    let mut policy_with_rule = value["workspace"]["policy"].clone();
    policy_with_rule["tag_rules"]["task"] = serde_json::json!({"allowed": [], "required": []});
    let policy_with_rule = serde_json::to_string(&policy_with_rule).unwrap();
    let policy_duplicate_tag_field =
        policy_with_rule.replacen("\"allowed\":[]", "\"allowed\":[],\"allowed\":[]", 1);
    let workspace_duplicate_tag_field = workspace.replacen(
        &format!("\"policy\":{policy}"),
        &format!("\"policy\":{policy_duplicate_tag_field}"),
        1,
    );
    let policy_duplicate_tag_rule = policy.replacen(
        "\"tag_rules\":{}",
        "\"tag_rules\":{\"task\":{\"allowed\":[],\"required\":[]},\"task\":{\"allowed\":[],\"required\":[]}}",
        1,
    );
    let workspace_duplicate_tag_rule = workspace.replacen(
        &format!("\"policy\":{policy}"),
        &format!("\"policy\":{policy_duplicate_tag_rule}"),
        1,
    );
    let cases = [
        format!(
            "{{\"format_version\":1,\"format_version\":1,\"workspace\":{workspace},\"documents\":{documents}}}"
        ),
        format!(
            "{{\"format_version\":1,\"workspace\":{workspace},\"workspace\":{workspace},\"documents\":{documents}}}"
        ),
        format!(
            "{{\"format_version\":1,\"workspace\":{workspace_duplicate_revision},\"documents\":{documents}}}"
        ),
        format!(
            "{{\"format_version\":1,\"workspace\":{workspace_duplicate_policy},\"documents\":{documents}}}"
        ),
        format!(
            "{{\"format_version\":1,\"workspace\":{workspace_duplicate_tag_field},\"documents\":{documents}}}"
        ),
        format!(
            "{{\"format_version\":1,\"workspace\":{workspace_duplicate_tag_rule},\"documents\":{documents}}}"
        ),
    ];

    for (index, raw) in cases.into_iter().enumerate() {
        let snapshot = dir.path().join(format!("duplicate-{index}"));
        copy_snapshot(&base, &snapshot);
        std::fs::write(snapshot.join("manifest.json"), raw).unwrap();
        let (candidate, storage) =
            test_context(&dir.path().join(format!("duplicate-{index}.db"))).await;
        let report = execute_command(
            &candidate,
            OrgOfflineCommand::ImportWorkspace {
                input: snapshot,
                mode: ImportMode::Create,
                actor_id: "duplicate-test".into(),
                operation_id: format!("duplicate-{index}"),
            },
        )
        .await;
        assert!(!report.ok, "accepted duplicate manifest case {index}");
        assert!(storage
            .session()
            .await
            .unwrap()
            .get_org_workspace(WORKSPACE_ID.parse().unwrap())
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn workspace_update_is_revision_safe_and_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (context, storage) = test_context(&dir.path().join("update.db")).await;
    let (first, second) = seed_workspace(&context).await;
    let snapshot = dir.path().join("update-snapshot");
    assert!(
        execute_command(
            &context,
            OrgOfflineCommand::ExportWorkspace {
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                output: snapshot.clone(),
            },
        )
        .await
        .ok
    );

    let mut manifest = read_manifest(&snapshot);
    manifest.workspace.description = "Imported metadata".into();
    let second_entry = manifest
        .documents
        .iter_mut()
        .find(|document| document.id.to_string() == SECOND_DOCUMENT_ID)
        .unwrap();
    let changed_second = second.replace("Opaque\tLF", "Imported second");
    std::fs::write(snapshot.join(&second_entry.file), changed_second.as_bytes()).unwrap();
    second_entry.content_hash = hash(changed_second.as_bytes());
    write_manifest(&snapshot, &manifest);

    let note_context = Arc::new(Context::new(
        storage.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    ));
    let router = note_mcp::mcp_router(note_context, Arc::new(context.clone()));
    let mcp_update = call_org_mcp(
        &router,
        "org_put_document",
        serde_json::json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "mcp-agent",
            "operation_id": "task7-mcp-revision-n-plus-one",
            "document_id": DOCUMENT_ID,
            "path": "inbox/tasks.org",
            "source": first.replace("Opaque  CRLF", "Concurrent change"),
            "expected_revision": 1,
            "lease_proofs": {}
        }),
    )
    .await;
    assert!(mcp_update.get("error").is_none(), "{mcp_update}");

    let workspace_id = WORKSPACE_ID.parse().unwrap();
    let document_id = DOCUMENT_ID.parse().unwrap();
    let item_id = ITEM_ID.parse().unwrap();
    let before_session = storage.session().await.unwrap();
    let document_before = before_session
        .get_org_document(document_id)
        .await
        .unwrap()
        .unwrap();
    let projection_before = before_session
        .list_org_workspace_projection(workspace_id)
        .await
        .unwrap();
    let leases_before = before_session
        .list_org_lease_history(item_id)
        .await
        .unwrap();
    let events_before = before_session
        .list_org_events(workspace_id, None, 200)
        .await
        .unwrap();
    let mcp_operation_before = before_session
        .get_org_operation(workspace_id, "task7-mcp-revision-n-plus-one")
        .await
        .unwrap();
    drop(before_session);

    let stale = execute_command(
        &context,
        OrgOfflineCommand::ImportWorkspace {
            input: snapshot.clone(),
            mode: ImportMode::Update,
            actor_id: "offline-update".into(),
            operation_id: "stale-update".into(),
        },
    )
    .await;
    assert!(!stale.ok);
    assert_eq!(stale.error.as_ref().unwrap().code, "stale_revision");
    assert_eq!(stale.error.as_ref().unwrap().details["current_revision"], 2);
    assert!(stale.documents.iter().all(|document| !document.applied));
    let session = storage.session().await.unwrap();
    assert_eq!(
        session
            .get_org_workspace(WORKSPACE_ID.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .description,
        "Portable snapshot"
    );
    assert_eq!(
        session
            .get_org_document(SECOND_DOCUMENT_ID.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .source,
        second
    );
    assert_eq!(
        session
            .get_org_document(document_id)
            .await
            .unwrap()
            .unwrap(),
        document_before
    );
    assert_eq!(
        session
            .list_org_workspace_projection(workspace_id)
            .await
            .unwrap(),
        projection_before
    );
    assert_eq!(
        session.list_org_lease_history(item_id).await.unwrap(),
        leases_before
    );
    assert_eq!(
        session
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap(),
        events_before
    );
    assert_eq!(
        session
            .get_org_operation(workspace_id, "task7-mcp-revision-n-plus-one")
            .await
            .unwrap(),
        mcp_operation_before
    );
    assert!(session
        .get_org_operation(workspace_id, "stale-update")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn active_lease_without_a_manifest_token_rejects_import_and_reports_no_token_material() {
    let dir = tempfile::tempdir().unwrap();
    let (context, _storage) = test_context(&dir.path().join("lease.db")).await;
    let (first, _second) = seed_workspace(&context).await;
    let claim = claim_item(
        &context,
        &envelope("claim-active"),
        &StartClaimRequest {
            work_item_id: ITEM_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let snapshot = dir.path().join("lease-snapshot");
    assert!(
        execute_command(
            &context,
            OrgOfflineCommand::ExportWorkspace {
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                output: snapshot.clone(),
            },
        )
        .await
        .ok
    );
    let mut manifest = read_manifest(&snapshot);
    let entry = manifest
        .documents
        .iter_mut()
        .find(|document| document.id.to_string() == DOCUMENT_ID)
        .unwrap();
    let changed = first
        .replace("READY", "RUNNING")
        .replace("Ship offline", "Changed under active lease");
    std::fs::write(snapshot.join(&entry.file), changed.as_bytes()).unwrap();
    entry.content_hash = hash(changed.as_bytes());
    write_manifest(&snapshot, &manifest);

    let report = execute_command(
        &context,
        OrgOfflineCommand::ImportWorkspace {
            input: snapshot,
            mode: ImportMode::Update,
            actor_id: "offline-update".into(),
            operation_id: "active-lease-update".into(),
        },
    )
    .await;
    assert!(!report.ok);
    let rendered = serde_json::to_string(&report).unwrap();
    assert!(!rendered.contains(&claim.fencing_token));
    assert!(!rendered.contains("fencing_token"));
    assert!(!rendered.contains("token_hash"));
}

#[tokio::test]
async fn document_import_create_and_update_enforce_existence_revision_and_raw_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (context, storage) = test_context(&dir.path().join("document.db")).await;
    create_workspace(
        &context,
        &envelope("document-workspace"),
        &CreateWorkspaceRequest {
            slug: "documents".into(),
            display_name: "Documents".into(),
            description: String::new(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: note_org::WorkspacePolicy::engineering_default(),
        },
    )
    .await
    .unwrap();
    let input = dir.path().join("input.org");
    let original = source(ITEM_ID, "\r\n", "Original  bytes");
    std::fs::write(&input, original.as_bytes()).unwrap();

    let created = execute_command(
        &context,
        OrgOfflineCommand::ImportDocument {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            path: "tasks/main.org".into(),
            input: input.clone(),
            mode: ImportMode::Create,
            actor_id: "offline-document".into(),
            operation_id: "document-create".into(),
            expected_revision: None,
        },
    )
    .await;
    assert!(created.ok, "{created:?}");
    assert_eq!(created.document_revisions[DOCUMENT_ID], 1);
    let duplicate = execute_command(
        &context,
        OrgOfflineCommand::ImportDocument {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            path: "tasks/main.org".into(),
            input: input.clone(),
            mode: ImportMode::Create,
            actor_id: "offline-document".into(),
            operation_id: "document-duplicate".into(),
            expected_revision: None,
        },
    )
    .await;
    assert!(!duplicate.ok);

    let updated_raw = original.replace("Original  bytes", "Updated\tbytes");
    std::fs::write(&input, updated_raw.as_bytes()).unwrap();
    let updated = execute_command(
        &context,
        OrgOfflineCommand::ImportDocument {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            path: "tasks/main.org".into(),
            input: input.clone(),
            mode: ImportMode::Update,
            actor_id: "offline-document".into(),
            operation_id: "document-update".into(),
            expected_revision: Some(1),
        },
    )
    .await;
    assert!(updated.ok, "{updated:?}");
    assert_eq!(updated.document_revisions[DOCUMENT_ID], 2);
    let stored = storage
        .session()
        .await
        .unwrap()
        .get_org_document(DOCUMENT_ID.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.source.as_bytes(), updated_raw.as_bytes());

    let stale = execute_command(
        &context,
        OrgOfflineCommand::ImportDocument {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            path: "tasks/main.org".into(),
            input,
            mode: ImportMode::Update,
            actor_id: "offline-document".into(),
            operation_id: "document-stale".into(),
            expected_revision: Some(1),
        },
    )
    .await;
    assert!(!stale.ok);
    assert_eq!(stale.error.unwrap().code, "stale_revision");
}

#[tokio::test]
async fn offline_document_import_keeps_unresolved_note_links_without_note_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (context, storage) = test_context(&dir.path().join("offline-note-links.db")).await;
    create_workspace(
        &context,
        &envelope("offline-note-link-workspace"),
        &CreateWorkspaceRequest {
            slug: "offline-note-links".into(),
            display_name: "Offline note links".into(),
            description: String::new(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: note_org::WorkspacePolicy::engineering_default(),
        },
    )
    .await
    .unwrap();
    let input = dir.path().join("note-link.org");
    let raw = source(
        ITEM_ID,
        "\n",
        "[[agent-note:design:40000000-0000-4000-8000-000000000099][Missing note]]",
    );
    std::fs::write(&input, raw).unwrap();

    let report = execute_command(
        &context,
        OrgOfflineCommand::ImportDocument {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            path: "offline/note-link.org".into(),
            input,
            mode: ImportMode::Create,
            actor_id: "offline-import".into(),
            operation_id: "offline-note-link".into(),
            expected_revision: None,
        },
    )
    .await;

    assert!(report.ok, "{report:?}");
    assert!(storage
        .session()
        .await
        .unwrap()
        .list_all_notes()
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn reopened_turso_offline_exports_preserve_ids_revisions_and_raw_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("offline-reopen.db");
    let (context, storage) = test_context(&database).await;
    let (first, second) = seed_workspace(&context).await;

    drop(context);
    drop(storage);

    let (reopened, _reopened_storage) = test_context(&database).await;
    let workspace_output = dir.path().join("reopened-workspace");
    let workspace_report = execute_command(
        &reopened,
        OrgOfflineCommand::ExportWorkspace {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            output: workspace_output.clone(),
        },
    )
    .await;
    assert!(workspace_report.ok, "workspace export failed after reopen");
    assert_eq!(
        workspace_report.workspace_id.unwrap().to_string(),
        WORKSPACE_ID
    );
    assert_eq!(workspace_report.document_revisions[DOCUMENT_ID], 1);
    assert_eq!(workspace_report.document_revisions[SECOND_DOCUMENT_ID], 1);

    let manifest = read_manifest(&workspace_output);
    assert_eq!(manifest.workspace.id.to_string(), WORKSPACE_ID);
    assert_eq!(manifest.workspace.revision, 1);
    let exported = manifest
        .documents
        .iter()
        .map(|document| {
            (
                document.id.to_string(),
                (
                    document.revision,
                    std::fs::read(workspace_output.join(&document.file)).unwrap(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(exported[DOCUMENT_ID].0, 1);
    assert_eq!(exported[DOCUMENT_ID].1, first.as_bytes());
    assert_eq!(exported[SECOND_DOCUMENT_ID].0, 1);
    assert_eq!(exported[SECOND_DOCUMENT_ID].1, second.as_bytes());

    let document_output = dir.path().join("reopened-document.org");
    let document_report = execute_command(
        &reopened,
        OrgOfflineCommand::ExportDocument {
            document_id: DOCUMENT_ID.parse().unwrap(),
            output: document_output.clone(),
        },
    )
    .await;
    assert!(document_report.ok, "document export failed after reopen");
    assert_eq!(
        document_report.document_id.unwrap().to_string(),
        DOCUMENT_ID
    );
    assert_eq!(document_report.document_revisions[DOCUMENT_ID], 1);
    assert_eq!(std::fs::read(document_output).unwrap(), first.as_bytes());
}

#[cfg(unix)]
#[tokio::test]
async fn document_import_rejects_a_symlinked_input_ancestor() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let (context, storage) = test_context(&dir.path().join("symlink-ancestor.db")).await;
    create_workspace(
        &context,
        &envelope("symlink-ancestor-workspace"),
        &CreateWorkspaceRequest {
            slug: "symlink-ancestor".into(),
            display_name: "Symlink ancestor".into(),
            description: String::new(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: note_org::WorkspacePolicy::engineering_default(),
        },
    )
    .await
    .unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("input.org"), source(ITEM_ID, "\n", "body")).unwrap();
    let linked = dir.path().join("linked");
    symlink(&real, &linked).unwrap();

    let report = execute_command(
        &context,
        OrgOfflineCommand::ImportDocument {
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            path: "offline/input.org".into(),
            input: linked.join("input.org"),
            mode: ImportMode::Create,
            actor_id: "offline-import".into(),
            operation_id: "symlink-ancestor".into(),
            expected_revision: None,
        },
    )
    .await;

    assert!(!report.ok);
    assert!(storage
        .session()
        .await
        .unwrap()
        .get_org_document(DOCUMENT_ID.parse().unwrap())
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn domain_failure_exits_nonzero_with_a_machine_readable_stdout_report() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        r#"[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "openai"
base_url = "http://127.0.0.1:9"
api_key_env = "MISSING_OFFLINE_SECRET"

[attachments]
engine = "filesystem"
path = "attachments"
"#,
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_note-server"))
        .args([
            "org",
            "export-document",
            "--document-id",
            "20000000-0000-4000-8000-000000000099",
            "--output",
            "missing.org",
        ])
        .current_dir(dir.path())
        .env("NOTE_CONFIG_PATH", &config)
        .env_remove("MISSING_OFFLINE_SECRET")
        .output()
        .await
        .unwrap();

    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["applied"], false);
    assert_eq!(report["error"]["code"], "not_found");
    assert!(!dir.path().join("attachments").exists());
}

#[tokio::test]
async fn every_org_startup_failure_is_a_sanitized_machine_readable_stdout_report() {
    let dir = tempfile::tempdir().unwrap();
    let malformed_config = dir.path().join("malformed.toml");
    std::fs::write(&malformed_config, "private_database_detail = [").unwrap();
    let storage_dir = dir.path().join("database-directory");
    std::fs::create_dir(&storage_dir).unwrap();
    let storage_config = dir.path().join("storage.toml");
    std::fs::write(
        &storage_config,
        format!(
            r#"[database]
engine = "embed"
path = "{}"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
"#,
            storage_dir.display()
        ),
    )
    .unwrap();

    let cases = [
        (
            vec!["org", "unknown", "--internal-role", "embedding-worker"],
            None,
            "embedding-worker",
        ),
        (
            vec!["org", "unknown", "--private-option", "private-value"],
            None,
            "private-value",
        ),
        (
            vec![
                "org",
                "export-document",
                "--document-id",
                DOCUMENT_ID,
                "--output",
                "out.org",
            ],
            Some(&malformed_config),
            "private_database_detail",
        ),
        (
            vec![
                "org",
                "export-document",
                "--document-id",
                DOCUMENT_ID,
                "--output",
                "out.org",
            ],
            Some(&storage_config),
            storage_dir.to_str().unwrap(),
        ),
    ];

    for (arguments, config, secret) in cases {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_note-server"));
        command.args(arguments).current_dir(dir.path());
        if let Some(config) = config {
            command.env("NOTE_CONFIG_PATH", config);
        } else {
            command.env_remove("NOTE_CONFIG_PATH");
        }
        let output = command.output().await.unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["applied"], false);
        assert!(report["error"]["code"].is_string());
        let rendered = String::from_utf8_lossy(&output.stdout);
        assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
    }
}
