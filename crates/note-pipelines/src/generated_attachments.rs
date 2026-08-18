use note_attachments::{AttachmentStore, PutObjectRequest};
use note_core::{AttachmentStorageMetadata, NoteAttachment};
use sha2::{Digest, Sha256};

const CHECKSUM_PREFIX_LEN: usize = 16;

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
            Err(_) => {
                return Err(published
                    .cleanup_with_primary(store, anyhow::anyhow!("attachment object write failed"))
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
