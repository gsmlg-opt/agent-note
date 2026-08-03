use note_org::{DocumentId, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewOrgDocument, NewOrgWorkspace, OrgDocumentUpdate, OrgWorkspaceUpdate,
    StorageBackend, StorageErrorKind,
};
use std::str::FromStr as _;
use std::sync::Arc;

fn workspace_id(value: &str) -> WorkspaceId {
    WorkspaceId::from_str(value).unwrap()
}

fn document_id(value: &str) -> DocumentId {
    DocumentId::from_str(value).unwrap()
}

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    let alpha_id = workspace_id("10000000-0000-0000-0000-000000000001");
    let beta_id = workspace_id("10000000-0000-0000-0000-000000000002");
    let missing_workspace_id = workspace_id("10000000-0000-0000-0000-000000000099");
    let policy = WorkspacePolicy::engineering_default();

    session
        .insert_org_workspace(NewOrgWorkspace {
            id: beta_id,
            slug: "beta",
            display_name: "Beta",
            description: "second workspace",
            timezone: "Asia/Shanghai",
            policy_schema_version: 1,
            policy: &policy,
            now: 10,
        })
        .await
        .unwrap();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: alpha_id,
            slug: "alpha",
            display_name: "Alpha",
            description: "first workspace",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 20,
        })
        .await
        .unwrap();

    let alpha = session.get_org_workspace(alpha_id).await.unwrap().unwrap();
    assert_eq!(alpha.slug, "alpha");
    assert_eq!(alpha.revision, 1);
    assert_eq!(alpha.created_at, 20);
    assert_eq!(alpha.updated_at, 20);
    assert_eq!(alpha.policy, policy);
    assert_eq!(
        session
            .get_org_workspace_by_slug("beta")
            .await
            .unwrap()
            .unwrap()
            .id,
        beta_id
    );

    let duplicate_slug = session
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_id("10000000-0000-0000-0000-000000000003"),
            slug: "alpha",
            display_name: "Duplicate",
            description: "duplicate slug",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 21,
        })
        .await
        .unwrap_err();
    assert_eq!(duplicate_slug.kind(), StorageErrorKind::Constraint);

    let source = "* TODO Unicode 任务\r\n#+begin_src opaque\r\n\\0 not parsed\r\n#+end_src";
    let revision_two_source =
        "* DONE Unicode 任务\r\n#+begin_src opaque\r\n\\0 still not parsed\r\n#+end_src";
    let first_document_id = document_id("20000000-0000-0000-0000-000000000001");
    let second_document_id = document_id("20000000-0000-0000-0000-000000000002");
    session
        .insert_org_document(NewOrgDocument {
            id: first_document_id,
            workspace_id: alpha_id,
            path: "zeta.org",
            source,
            content_hash: "hash-one",
            now: 30,
        })
        .await
        .unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: second_document_id,
            workspace_id: alpha_id,
            path: "alpha.org",
            source: "* TODO Other",
            content_hash: "hash-other",
            now: 31,
        })
        .await
        .unwrap();

    let stored = session
        .get_org_document(first_document_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.source.as_bytes(), source.as_bytes());
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.created_at, 30);
    assert_eq!(stored.updated_at, 30);
    let documents = session.list_org_documents(alpha_id).await.unwrap();
    assert_eq!(
        documents
            .iter()
            .map(|document| (document.path.as_str(), document.id))
            .collect::<Vec<_>>(),
        vec![
            ("alpha.org", second_document_id),
            ("zeta.org", first_document_id)
        ]
    );

    let duplicate_path = session
        .insert_org_document(NewOrgDocument {
            id: document_id("20000000-0000-0000-0000-000000000003"),
            workspace_id: alpha_id,
            path: "zeta.org",
            source: "duplicate",
            content_hash: "duplicate-hash",
            now: 32,
        })
        .await
        .unwrap_err();
    assert_eq!(duplicate_path.kind(), StorageErrorKind::Constraint);

    let document_update = OrgDocumentUpdate {
        id: first_document_id,
        expected_revision: 1,
        path: "zeta.org",
        source: revision_two_source,
        content_hash: "hash-two",
        updated_at: 40,
    };
    let CompareAndSwap::Applied(updated_document) = session
        .compare_and_swap_org_document(document_update)
        .await
        .unwrap()
    else {
        panic!("document compare-and-swap was not applied");
    };
    assert_eq!(updated_document.revision, 2);
    assert_eq!(
        updated_document.source.as_bytes(),
        revision_two_source.as_bytes()
    );
    assert_eq!(updated_document.created_at, 30);
    assert_eq!(updated_document.updated_at, 40);

    assert_eq!(
        session
            .compare_and_swap_org_document(OrgDocumentUpdate {
                id: first_document_id,
                expected_revision: 1,
                path: "stale.org",
                source: "stale",
                content_hash: "stale-hash",
                updated_at: 41,
            })
            .await
            .unwrap(),
        CompareAndSwap::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(
        session
            .get_org_document(first_document_id)
            .await
            .unwrap()
            .unwrap()
            .source
            .as_bytes(),
        revision_two_source.as_bytes()
    );
    assert_eq!(
        session
            .compare_and_swap_org_document(OrgDocumentUpdate {
                id: document_id("20000000-0000-0000-0000-000000000099"),
                expected_revision: 1,
                path: "missing.org",
                source: "missing",
                content_hash: "missing-hash",
                updated_at: 42,
            })
            .await
            .unwrap(),
        CompareAndSwap::NotFound
    );

    let CompareAndSwap::Applied(archived_alpha) = session
        .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
            id: alpha_id,
            expected_revision: 1,
            slug: "alpha",
            display_name: "Alpha archived",
            description: "first workspace archived",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            archived_at: Some(50),
            updated_at: 50,
        })
        .await
        .unwrap()
    else {
        panic!("workspace compare-and-swap was not applied");
    };
    assert_eq!(archived_alpha.revision, 2);
    assert_eq!(archived_alpha.created_at, 20);
    assert_eq!(archived_alpha.updated_at, 50);
    assert_eq!(archived_alpha.archived_at, Some(50));

    assert_eq!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: alpha_id,
                expected_revision: 1,
                slug: "alpha-stale",
                display_name: "Stale",
                description: "stale",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                archived_at: None,
                updated_at: 51,
            })
            .await
            .unwrap(),
        CompareAndSwap::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: missing_workspace_id,
                expected_revision: 1,
                slug: "missing",
                display_name: "Missing",
                description: "missing",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                archived_at: None,
                updated_at: 52,
            })
            .await
            .unwrap(),
        CompareAndSwap::NotFound
    );

    let active = session.list_org_workspaces(false).await.unwrap();
    assert_eq!(
        active
            .iter()
            .map(|workspace| (workspace.slug.as_str(), workspace.id))
            .collect::<Vec<_>>(),
        vec![("beta", beta_id)]
    );
    let all = session.list_org_workspaces(true).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|workspace| (workspace.slug.as_str(), workspace.id))
            .collect::<Vec<_>>(),
        vec![("alpha", alpha_id), ("beta", beta_id)]
    );
}
