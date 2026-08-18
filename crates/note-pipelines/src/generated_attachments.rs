use note_attachments::{AttachmentStore, PutObjectRequest};
use note_core::{normalize_attachment_path, AttachmentStorageMetadata, NoteAttachment};
use note_storage::{
    AttachmentOperationKind, AttachmentOperationStatus, NewAttachmentOperation, StorageTransaction,
};
use sha2::{Digest, Sha256};

const CHECKSUM_PREFIX_LEN: usize = 16;
const LEGACY_STORAGE_GENERATION: &str = "legacy-path-v1";
const CLEANUP_CLAIM_LIMIT: i64 = 32;
const CLEANUP_LEASE_SECONDS: i64 = 30;
const CLEANUP_RETRY_SECONDS: i64 = 60;
const CLEANUP_FAILURE: &str = "attachment cleanup failed; retry pending";

pub(crate) struct PublishedAttachmentSet {
    pub(crate) attachments: Vec<NoteAttachment>,
    pub(crate) metadata: Vec<NoteAttachment>,
    object_keys: Vec<String>,
}

pub(crate) async fn publish_generated_attachments(
    store: &dyn AttachmentStore,
    note_id: &str,
    attachments: &[NoteAttachment],
) -> anyhow::Result<PublishedAttachmentSet> {
    let mut published = PublishedAttachmentSet {
        attachments: Vec::with_capacity(attachments.len()),
        metadata: Vec::with_capacity(attachments.len()),
        object_keys: Vec::with_capacity(attachments.len()),
    };

    for attachment in attachments {
        let checksum_sha256 = format!("{:x}", Sha256::digest(&attachment.content));
        let storage_generation = uuid::Uuid::new_v4().to_string();
        let object_key = format!(
            "notes/{note_id}/objects/{storage_generation}-{}",
            &checksum_sha256[..CHECKSUM_PREFIX_LEN]
        );
        let size_bytes = attachment.content.len() as u64;
        published.object_keys.push(object_key.clone());

        let stored = match store
            .put_immutable(PutObjectRequest {
                object_key: object_key.clone(),
                bytes: attachment.content.clone(),
                checksum_sha256: checksum_sha256.clone(),
            })
            .await
        {
            Ok(stored) => stored,
            Err(error) => {
                return Err(published
                    .cleanup_with_primary(store, error.context("attachment object write failed"))
                    .await);
            }
        };

        if stored.object_key != object_key
            || stored.size_bytes != size_bytes
            || stored.checksum_sha256 != checksum_sha256
        {
            return Err(published
                .cleanup_with_primary(
                    store,
                    anyhow::anyhow!("attachment object verification failed"),
                )
                .await);
        }

        let storage = Some(AttachmentStorageMetadata {
            object_key,
            storage_generation,
            size_bytes,
            checksum_sha256,
        });
        let mut hydrated = attachment.clone();
        hydrated.storage = storage.clone();
        published.attachments.push(hydrated);
        published.metadata.push(NoteAttachment {
            id: attachment.id.clone(),
            path: attachment.path.clone(),
            mime: attachment.mime.clone(),
            description: attachment.description.clone(),
            content: Vec::new(),
            storage,
        });
    }

    Ok(published)
}

impl PublishedAttachmentSet {
    pub(crate) async fn cleanup_with_primary(
        self,
        store: &dyn AttachmentStore,
        primary: anyhow::Error,
    ) -> anyhow::Error {
        let mut cleanup_failed = false;
        for object_key in self.object_keys {
            if store.delete_object(&object_key).await.is_err() {
                cleanup_failed = true;
            }
        }
        if cleanup_failed {
            primary.context("generated attachment cleanup also failed; safe orphan may remain")
        } else {
            primary
        }
    }
}

pub(crate) async fn enqueue_attachment_cleanup(
    transaction: &dyn StorageTransaction,
    note_id: &str,
    attachments: &[NoteAttachment],
    now: i64,
) -> anyhow::Result<()> {
    for attachment in attachments {
        let (storage_generation, object_key) = match &attachment.storage {
            Some(storage) => (
                storage.storage_generation.clone(),
                storage.object_key.clone(),
            ),
            None => (
                LEGACY_STORAGE_GENERATION.to_string(),
                normalize_attachment_path(&attachment.path),
            ),
        };
        transaction
            .insert_attachment_operation(NewAttachmentOperation {
                id: uuid::Uuid::new_v4().to_string(),
                kind: AttachmentOperationKind::DeleteObject,
                note_id: note_id.to_string(),
                attachment_id: attachment.id.clone(),
                storage_generation,
                object_key,
                next_attempt_at: Some(now),
                created_at: now,
            })
            .await?;
    }
    Ok(())
}

pub(crate) async fn run_attachment_cleanup_once(ctx: &crate::Context) {
    let now = chrono::Utc::now().timestamp();
    let owner = format!("mutation-cleanup-{}", uuid::Uuid::new_v4());
    let Ok(session) = ctx.storage().session().await else {
        return;
    };
    let Ok(operations) = session
        .claim_attachment_operations(
            &owner,
            now,
            now.saturating_add(CLEANUP_LEASE_SECONDS),
            CLEANUP_CLAIM_LIMIT,
        )
        .await
    else {
        return;
    };

    for operation in operations {
        let deleted = if operation.storage_generation == LEGACY_STORAGE_GENERATION {
            ctx.attachments()
                .delete_legacy(&operation.note_id, &operation.object_key)
                .await
        } else {
            ctx.attachments().delete_object(&operation.object_key).await
        };
        match deleted {
            Ok(_) => {
                let _ = session
                    .complete_attachment_operation(
                        &operation.id,
                        &owner,
                        operation.attempts,
                        chrono::Utc::now().timestamp().max(now),
                    )
                    .await;
            }
            Err(_) => {
                let failed_at = chrono::Utc::now().timestamp().max(now);
                let _ = session
                    .fail_attachment_operation(
                        &operation.id,
                        &owner,
                        operation.attempts,
                        AttachmentOperationStatus::Pending,
                        Some(failed_at.saturating_add(CLEANUP_RETRY_SECONDS)),
                        CLEANUP_FAILURE,
                        failed_at,
                    )
                    .await;
            }
        }
    }
}
