use crate::{
    path::{canonical_attachment_path, note_directory},
    AttachmentStore, AttachmentStoreInfo, PreparedAttachmentSet,
};
use note_core::NoteAttachment;
use std::{
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::{
    fs,
    sync::{Mutex, OwnedMutexGuard},
};

/// Filesystem-backed attachment storage.
///
/// The configured root must be exclusively owned by this application. Callers
/// must not permit external mutation or creation of symlinks below the root.
/// Symlinks are checked before reads, removal, and replacement, but those checks
/// are not a capability-based defense against a concurrent filesystem attacker.
#[derive(Clone)]
pub struct FilesystemAttachmentStore {
    root: PathBuf,
    coordination: Arc<Mutex<()>>,
}

struct PreparedFilesystemSet {
    final_dir: PathBuf,
    staging: Option<DirectoryCleanupGuard>,
    metadata: Vec<NoteAttachment>,
    _coordination_guard: OwnedMutexGuard<()>,
}

struct DirectoryCleanupGuard {
    path: Option<PathBuf>,
}

struct PublicationRollback {
    final_dir: PathBuf,
    backup_dir: Option<PathBuf>,
}

impl FilesystemAttachmentStore {
    /// Creates a store rooted at an application-exclusive directory.
    ///
    /// The caller must ensure no other process or user mutates this directory
    /// or places symlinks below it while the store is in use.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            coordination: Arc::new(Mutex::new(())),
        }
    }

    async fn read_while_locked(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        let note_dir = note_directory(&self.root, note_id)?;
        if !existing_safe_directory(&note_dir).await? {
            anyhow::bail!("attachment note directory does not exist");
        }
        let path_on_disk = canonical_attachment_path(&note_dir, path)?;
        reject_symlink_components(&note_dir, path).await?;
        Ok(fs::read(path_on_disk).await?)
    }
}

#[async_trait::async_trait]
impl AttachmentStore for FilesystemAttachmentStore {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>> {
        let coordination_guard = self.coordination.clone().lock_owned().await;
        let final_dir = note_directory(&self.root, note_id)?;
        let metadata = attachments
            .iter()
            .map(|attachment| NoteAttachment {
                id: attachment.id.clone(),
                path: attachment.path.clone(),
                mime: attachment.mime.clone(),
                description: attachment.description.clone(),
                content: Vec::new(),
            })
            .collect();

        if attachments.is_empty() {
            return Ok(Box::new(PreparedFilesystemSet {
                final_dir,
                staging: None,
                metadata,
                _coordination_guard: coordination_guard,
            }));
        }

        fs::create_dir_all(&self.root).await?;
        let temp_dir = self
            .root
            .join(format!(".{note_id}-{}.tmp", uuid::Uuid::new_v4()));
        let mut staging = DirectoryCleanupGuard::new(temp_dir);
        fs::create_dir(staging.path()).await?;

        let write_result = async {
            for attachment in attachments {
                let path = canonical_attachment_path(staging.path(), &attachment.path)?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).await?;
                }
                fs::write(path, &attachment.content).await?;
            }
            anyhow::Ok(())
        }
        .await;

        if let Err(error) = write_result {
            let _ = staging.cleanup().await;
            return Err(error);
        }

        Ok(Box::new(PreparedFilesystemSet {
            final_dir,
            staging: Some(staging),
            metadata,
            _coordination_guard: coordination_guard,
        }))
    }

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        let _guard = self.coordination.lock().await;
        self.read_while_locked(note_id, path).await
    }

    async fn hydrate(
        &self,
        note_id: &str,
        attachments: &mut [NoteAttachment],
    ) -> anyhow::Result<()> {
        let _guard = self.coordination.lock().await;
        for attachment in attachments {
            attachment.content = self.read_while_locked(note_id, &attachment.path).await?;
        }
        Ok(())
    }

    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
        let _guard = self.coordination.lock().await;
        let note_dir = note_directory(&self.root, note_id)?;
        if existing_safe_directory(&note_dir).await? {
            remove_directory_if_present(&note_dir).await?;
        }
        Ok(())
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "filesystem".to_string(),
            location: Some(self.root.to_string_lossy().into_owned()),
        }
    }
}

#[async_trait::async_trait]
impl PreparedAttachmentSet for PreparedFilesystemSet {
    fn metadata(&self) -> &[NoteAttachment] {
        &self.metadata
    }

    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        let Self {
            final_dir,
            mut staging,
            metadata: _,
            _coordination_guard,
        } = *self;
        let backup_dir = final_dir.with_file_name(format!(
            ".{}-{}.backup",
            final_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("attachments"),
            uuid::Uuid::new_v4()
        ));
        let mut rollback = PublicationRollback::new(final_dir.clone());

        if existing_safe_directory(&final_dir).await? {
            fs::rename(&final_dir, &backup_dir).await?;
            rollback.arm(backup_dir);
        }

        if let Some(staging) = staging.as_mut() {
            if let Err(promotion_error) = fs::rename(staging.path(), &final_dir).await {
                let cleanup_error = staging.cleanup().await.err();
                let restore_error = rollback.restore().await.err();
                return Err(publication_error(
                    promotion_error,
                    cleanup_error,
                    restore_error,
                ));
            }
            staging.disarm();
        }

        if let Some(backup_dir) = rollback.release() {
            let mut obsolete_backup = DirectoryCleanupGuard::new(backup_dir);
            obsolete_backup.cleanup().await?;
        }
        drop(_coordination_guard);
        Ok(())
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        let Self {
            final_dir: _,
            mut staging,
            metadata: _,
            _coordination_guard,
        } = *self;
        if let Some(staging) = staging.as_mut() {
            staging.cleanup().await?;
        }
        drop(_coordination_guard);
        Ok(())
    }
}

impl DirectoryCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("armed directory cleanup guard")
    }

    fn disarm(&mut self) {
        self.path = None;
    }

    async fn cleanup(&mut self) -> anyhow::Result<()> {
        if let Some(path) = self.path.as_deref() {
            remove_directory_if_present(path).await?;
            self.disarm();
        }
        Ok(())
    }
}

impl Drop for DirectoryCleanupGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

impl PublicationRollback {
    fn new(final_dir: PathBuf) -> Self {
        Self {
            final_dir,
            backup_dir: None,
        }
    }

    fn arm(&mut self, backup_dir: PathBuf) {
        self.backup_dir = Some(backup_dir);
    }

    async fn restore(&mut self) -> anyhow::Result<()> {
        if let Some(backup_dir) = self.backup_dir.as_deref() {
            fs::rename(backup_dir, &self.final_dir).await?;
            self.backup_dir = None;
        }
        Ok(())
    }

    fn release(&mut self) -> Option<PathBuf> {
        self.backup_dir.take()
    }
}

impl Drop for PublicationRollback {
    fn drop(&mut self) {
        let Some(backup_dir) = self.backup_dir.take() else {
            return;
        };
        if !self.final_dir.exists() {
            let _ = std::fs::rename(backup_dir, &self.final_dir);
        }
    }
}

async fn existing_safe_directory(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("attachment directory must not be a symlink")
        }
        Ok(metadata) if !metadata.is_dir() => {
            anyhow::bail!("attachment directory must be a directory")
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn reject_symlink_components(note_dir: &Path, attachment_path: &str) -> anyhow::Result<()> {
    let mut path = note_dir.to_path_buf();
    for component in Path::new(attachment_path).components() {
        match component {
            Component::Normal(part) => {
                path.push(part);
                match fs::symlink_metadata(&path).await {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        anyhow::bail!("attachment path must not contain a symlink")
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
            }
            Component::CurDir => {}
            _ => anyhow::bail!("invalid attachment path"),
        }
    }
    Ok(())
}

async fn remove_directory_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn publication_error(
    promotion_error: std::io::Error,
    cleanup_error: Option<anyhow::Error>,
    restore_error: Option<anyhow::Error>,
) -> anyhow::Error {
    let mut message = format!("failed to promote staged attachments: {promotion_error}");
    if let Some(error) = cleanup_error {
        message.push_str(&format!("; failed to clean staged attachments: {error}"));
    }
    if let Some(error) = restore_error {
        message.push_str(&format!(
            "; failed to restore previous attachments: {error}"
        ));
    }
    anyhow::anyhow!(message)
}
