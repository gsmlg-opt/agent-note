use crate::{
    path::{canonical_relative_path, note_directory},
    AttachmentStore, AttachmentStoreInfo, PreparedAttachmentSet,
};
use note_core::NoteAttachment;
use std::{
    collections::HashMap,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, Weak},
};
use tokio::{
    fs,
    sync::{Mutex as AsyncMutex, OwnedMutexGuard},
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
    coordination: Arc<NoteCoordination>,
}

#[derive(Default)]
struct NoteCoordination {
    locks: StdMutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>,
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
    replacement_selected: bool,
}

impl NoteCoordination {
    fn lock_for(&self, note_dir: &Path) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(note_dir).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(note_dir.to_path_buf(), Arc::downgrade(&lock));
        lock
    }
}

impl FilesystemAttachmentStore {
    /// Creates a store rooted at an application-exclusive directory.
    ///
    /// The caller must ensure no other process or user mutates this directory
    /// or places symlinks below it while the store is in use.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            coordination: Arc::new(NoteCoordination::default()),
        }
    }

    async fn read_while_locked(&self, note_dir: &Path, path: &str) -> anyhow::Result<Vec<u8>> {
        if !existing_safe_directory(note_dir).await? {
            anyhow::bail!("attachment note directory does not exist");
        }
        let relative_path = canonical_relative_path(path)?;
        let path_on_disk = attachment_path_on_disk(note_dir, &relative_path);
        reject_symlink_components(note_dir, &relative_path).await?;
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
        let final_dir = note_directory(&self.root, note_id)?;
        let coordination_guard = self.coordination.lock_for(&final_dir).lock_owned().await;
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
                let relative_path = canonical_relative_path(&attachment.path)?;
                let path = attachment_path_on_disk(staging.path(), &relative_path);
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
        let note_dir = note_directory(&self.root, note_id)?;
        let note_lock = self.coordination.lock_for(&note_dir);
        let _guard = note_lock.lock().await;
        self.read_while_locked(&note_dir, path).await
    }

    async fn hydrate(
        &self,
        note_id: &str,
        attachments: &mut [NoteAttachment],
    ) -> anyhow::Result<()> {
        let note_dir = note_directory(&self.root, note_id)?;
        let note_lock = self.coordination.lock_for(&note_dir);
        let _guard = note_lock.lock().await;
        for attachment in attachments {
            attachment.content = self.read_while_locked(&note_dir, &attachment.path).await?;
        }
        Ok(())
    }

    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
        let note_dir = note_directory(&self.root, note_id)?;
        let note_lock = self.coordination.lock_for(&note_dir);
        let _guard = note_lock.lock().await;
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
        let prepared = *self;
        tokio::task::spawn_blocking(move || publish_blocking(prepared))
            .await
            .map_err(|error| anyhow::anyhow!("attachment publication task failed: {error}"))?
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

fn publish_blocking(prepared: PreparedFilesystemSet) -> anyhow::Result<()> {
    let PreparedFilesystemSet {
        final_dir,
        mut staging,
        metadata: _,
        _coordination_guard,
    } = prepared;
    let backup_dir = final_dir.with_file_name(format!(
        ".{}-{}.backup",
        final_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachments"),
        uuid::Uuid::new_v4()
    ));
    let mut rollback = PublicationRollback::new(final_dir.clone());

    if existing_safe_directory_blocking(&final_dir)? {
        std::fs::rename(&final_dir, &backup_dir)?;
        rollback.arm(backup_dir);
        #[cfg(test)]
        pause_publication(&final_dir, PublicationPhase::AfterBackup);
    }

    if let Some(staging) = staging.as_mut() {
        if let Err(promotion_error) = std::fs::rename(staging.path(), &final_dir) {
            let cleanup_error = staging.cleanup_blocking().err();
            let restore_error = rollback.restore_blocking().err();
            return Err(publication_error(
                promotion_error,
                cleanup_error,
                restore_error,
            ));
        }
        staging.disarm();
        rollback.mark_replacement_selected();
        #[cfg(test)]
        pause_publication(&final_dir, PublicationPhase::AfterPromotion);
    } else {
        rollback.mark_replacement_selected();
    }

    if let Some(backup_dir) = rollback.release() {
        let mut obsolete_backup = DirectoryCleanupGuard::new(backup_dir);
        obsolete_backup.cleanup_blocking()?;
    }
    drop(_coordination_guard);
    Ok(())
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

    fn cleanup_blocking(&mut self) -> anyhow::Result<()> {
        if let Some(path) = self.path.as_deref() {
            remove_directory_if_present_blocking(path)?;
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
            replacement_selected: false,
        }
    }

    fn arm(&mut self, backup_dir: PathBuf) {
        self.backup_dir = Some(backup_dir);
    }

    fn restore_blocking(&mut self) -> anyhow::Result<()> {
        if let Some(backup_dir) = self.backup_dir.as_deref() {
            std::fs::rename(backup_dir, &self.final_dir)?;
            self.backup_dir = None;
        }
        Ok(())
    }

    fn mark_replacement_selected(&mut self) {
        self.replacement_selected = true;
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
        if self.replacement_selected {
            let _ = std::fs::remove_dir_all(backup_dir);
        } else if !self.final_dir.exists() {
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

fn existing_safe_directory_blocking(path: &Path) -> anyhow::Result<bool> {
    match std::fs::symlink_metadata(path) {
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

fn attachment_path_on_disk(root: &Path, relative_path: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative_path.split('/') {
        path.push(segment);
    }
    path
}

async fn reject_symlink_components(note_dir: &Path, attachment_path: &str) -> anyhow::Result<()> {
    let mut path = note_dir.to_path_buf();
    for segment in attachment_path.split('/') {
        path.push(segment);
        match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("attachment path must not contain a symlink")
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
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

fn remove_directory_if_present_blocking(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_dir_all(path) {
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

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationPhase {
    AfterBackup,
    AfterPromotion,
}

#[cfg(test)]
struct PublicationPause {
    final_dir: PathBuf,
    phase: PublicationPhase,
    reached: tokio::sync::oneshot::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
fn publication_pauses() -> &'static StdMutex<Vec<PublicationPause>> {
    static PAUSES: std::sync::OnceLock<StdMutex<Vec<PublicationPause>>> =
        std::sync::OnceLock::new();
    PAUSES.get_or_init(|| StdMutex::new(Vec::new()))
}

#[cfg(test)]
fn install_publication_pause(
    final_dir: PathBuf,
    phase: PublicationPhase,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (reached_sender, reached_receiver) = tokio::sync::oneshot::channel();
    let (resume_sender, resume_receiver) = std::sync::mpsc::channel();
    publication_pauses()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(PublicationPause {
            final_dir,
            phase,
            reached: reached_sender,
            resume: resume_receiver,
        });
    (reached_receiver, resume_sender)
}

#[cfg(test)]
fn pause_publication(final_dir: &Path, phase: PublicationPhase) {
    let pause = {
        let mut pauses = publication_pauses()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pauses
            .iter()
            .position(|pause| pause.final_dir == final_dir && pause.phase == phase)
            .map(|index| pauses.swap_remove(index))
    };
    if let Some(pause) = pause {
        let _ = pause.reached.send(());
        let _ = pause.resume.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn attachment(content: &[u8]) -> NoteAttachment {
        NoteAttachment {
            id: "file".to_string(),
            path: "./file.txt".to_string(),
            mime: "text/plain".to_string(),
            description: String::new(),
            content: content.to_vec(),
        }
    }

    fn staging_directory(root: &Path) -> PathBuf {
        std::fs::read_dir(root)
            .expect("read attachment root")
            .map(|entry| entry.expect("read directory entry").path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".tmp"))
            })
            .expect("staging directory")
    }

    fn assert_only_final_directory(root: &Path, final_dir: &Path) {
        let entries = std::fs::read_dir(root)
            .expect("read attachment root")
            .map(|entry| entry.expect("read directory entry").path())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![final_dir]);
    }

    #[tokio::test]
    async fn cancellation_after_backup_finishes_rollback_on_failed_promotion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        let final_dir = root.join("note-1");
        let store = FilesystemAttachmentStore::new(root.clone());
        store
            .prepare("note-1", &[attachment(b"old")])
            .await
            .unwrap()
            .publish()
            .await
            .unwrap();
        let prepared = store
            .prepare("note-1", &[attachment(b"new")])
            .await
            .unwrap();
        let staging = staging_directory(&root);
        let (reached, resume) =
            install_publication_pause(final_dir.clone(), PublicationPhase::AfterBackup);
        let publication = tokio::spawn(async move { prepared.publish().await });

        tokio::time::timeout(Duration::from_secs(2), reached)
            .await
            .expect("publication must reach backup phase")
            .expect("publication pause sender");
        std::fs::remove_dir_all(staging).unwrap();
        publication.abort();
        let join_error = publication
            .await
            .expect_err("outer publication task must be cancelled");
        assert!(join_error.is_cancelled());
        resume.send(()).expect("resume publication");

        let content =
            tokio::time::timeout(Duration::from_secs(2), store.read("note-1", "./file.txt"))
                .await
                .expect("rollback must release note lock")
                .unwrap();
        assert_eq!(content, b"old");
        assert_only_final_directory(&root, &final_dir);
    }

    #[tokio::test]
    async fn cancellation_after_promotion_finishes_backup_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        let final_dir = root.join("note-1");
        let store = FilesystemAttachmentStore::new(root.clone());
        store
            .prepare("note-1", &[attachment(b"old")])
            .await
            .unwrap()
            .publish()
            .await
            .unwrap();
        let prepared = store
            .prepare("note-1", &[attachment(b"new")])
            .await
            .unwrap();
        let (reached, resume) =
            install_publication_pause(final_dir.clone(), PublicationPhase::AfterPromotion);
        let publication = tokio::spawn(async move { prepared.publish().await });

        tokio::time::timeout(Duration::from_secs(2), reached)
            .await
            .expect("publication must reach promotion phase")
            .expect("publication pause sender");
        publication.abort();
        let join_error = publication
            .await
            .expect_err("outer publication task must be cancelled");
        assert!(join_error.is_cancelled());
        resume.send(()).expect("resume publication");

        let content =
            tokio::time::timeout(Duration::from_secs(2), store.read("note-1", "./file.txt"))
                .await
                .expect("cleanup must release note lock")
                .unwrap();
        assert_eq!(content, b"new");
        assert_only_final_directory(&root, &final_dir);
    }
}
