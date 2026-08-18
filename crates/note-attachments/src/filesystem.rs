use crate::{
    path::{canonical_object_key, canonical_relative_path, note_directory},
    AttachmentStore, AttachmentStoreInfo, DeleteObjectOutcome, ObjectMetadata,
    PreparedAttachmentMutation, PreparedAttachmentSet, PutObjectRequest, StoredObject,
};
use note_core::NoteAttachment;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{ErrorKind, Write},
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

struct PreparedFilesystemPut {
    note_dir: PathBuf,
    relative_path: String,
    staging: FileCleanupGuard,
    _coordination_guard: OwnedMutexGuard<()>,
}

struct PreparedFilesystemDelete {
    note_dir: PathBuf,
    relative_path: String,
    final_path: PathBuf,
    _coordination_guard: OwnedMutexGuard<()>,
}

struct DirectoryCleanupGuard {
    path: Option<PathBuf>,
}

struct FileCleanupGuard {
    path: Option<PathBuf>,
}

struct CreatedDirectoriesCleanupGuard {
    paths: Vec<PathBuf>,
}

struct PublicationRollback {
    final_dir: PathBuf,
    backup_dir: Option<PathBuf>,
    replacement_selected: bool,
}

struct FilePublicationRollback {
    final_path: PathBuf,
    backup_path: Option<PathBuf>,
    replacement_selected: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PutFailurePhase {
    Promotion,
    Restore,
    BackupCleanup,
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
    async fn put_immutable(&self, request: PutObjectRequest) -> anyhow::Result<StoredObject> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || put_immutable_blocking(&root, request))
            .await
            .map_err(|_| anyhow::anyhow!("attachment object put task failed"))?
    }

    async fn read_object(&self, object_key: &str) -> anyhow::Result<Vec<u8>> {
        let object_key = canonical_object_key(object_key)?;
        if !existing_safe_directory(&self.root).await? {
            anyhow::bail!("attachment object root does not exist");
        }
        reject_symlink_components(&self.root, &object_key).await?;
        Ok(fs::read(attachment_path_on_disk(&self.root, &object_key)).await?)
    }

    async fn head_object(&self, object_key: &str) -> anyhow::Result<ObjectMetadata> {
        let object_key = canonical_object_key(object_key)?;
        if !existing_safe_directory(&self.root).await? {
            anyhow::bail!("attachment object root does not exist");
        }
        reject_symlink_components(&self.root, &object_key).await?;
        let bytes = fs::read(attachment_path_on_disk(&self.root, &object_key)).await?;
        Ok(object_metadata(&bytes))
    }

    async fn delete_object(&self, object_key: &str) -> anyhow::Result<DeleteObjectOutcome> {
        let object_key = canonical_object_key(object_key)?;
        if !existing_safe_directory(&self.root).await? {
            return Ok(DeleteObjectOutcome::AlreadyAbsent);
        }
        reject_symlink_components(&self.root, &object_key).await?;
        match fs::remove_file(attachment_path_on_disk(&self.root, &object_key)).await {
            Ok(()) => Ok(DeleteObjectOutcome::Deleted),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Ok(DeleteObjectOutcome::AlreadyAbsent)
            }
            Err(error) => Err(error.into()),
        }
    }

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

    async fn prepare_put(
        &self,
        note_id: &str,
        attachment: &NoteAttachment,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        let note_dir = note_directory(&self.root, note_id)?;
        let coordination_guard = self.coordination.lock_for(&note_dir).lock_owned().await;
        let relative_path = canonical_relative_path(&attachment.path)?;
        if existing_safe_directory(&note_dir).await? {
            reject_symlink_components(&note_dir, &relative_path).await?;
        }

        fs::create_dir_all(&self.root).await?;
        let staging_path = self
            .root
            .join(format!(".{note_id}-{}.put.tmp", uuid::Uuid::new_v4()));
        let mut staging = FileCleanupGuard::new(staging_path);
        if let Err(error) = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.path())
            .await
        {
            let _ = staging.cleanup().await;
            return Err(error.into());
        }
        #[cfg(test)]
        pause_prepare_put(&note_dir, staging.path()).await;
        if let Err(error) = fs::write(staging.path(), &attachment.content).await {
            let _ = staging.cleanup().await;
            return Err(error.into());
        }

        Ok(Box::new(PreparedFilesystemPut {
            note_dir,
            relative_path,
            staging,
            _coordination_guard: coordination_guard,
        }))
    }

    async fn prepare_delete(
        &self,
        note_id: &str,
        path: &str,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        let note_dir = note_directory(&self.root, note_id)?;
        let coordination_guard = self.coordination.lock_for(&note_dir).lock_owned().await;
        let relative_path = canonical_relative_path(path)?;
        if existing_safe_directory(&note_dir).await? {
            reject_symlink_components(&note_dir, &relative_path).await?;
        }
        let final_path = attachment_path_on_disk(&note_dir, &relative_path);

        Ok(Box::new(PreparedFilesystemDelete {
            note_dir,
            relative_path,
            final_path,
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

fn put_immutable_blocking(root: &Path, request: PutObjectRequest) -> anyhow::Result<StoredObject> {
    let object_key = canonical_object_key(&request.object_key)?;
    let expected_checksum = validate_sha256(&request.checksum_sha256)?;
    let actual = object_metadata(&request.bytes);
    if actual.checksum_sha256 != expected_checksum {
        anyhow::bail!("attachment object checksum does not match expected SHA-256");
    }

    let mut created = ensure_safe_parent_directories_blocking(root, &object_key)?;
    let final_path = attachment_path_on_disk(root, &object_key);
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid attachment object key"))?;
    let temp_path = final_path.with_file_name(format!(".{file_name}-{}.tmp", uuid::Uuid::new_v4()));
    let mut temp = FileCleanupGuard::new(temp_path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp.path())?;
    file.write_all(&request.bytes)?;
    file.flush()?;
    drop(file);

    if let Err(error) = rename_noreplace(temp.path(), &final_path) {
        return if error.kind() == ErrorKind::AlreadyExists {
            Err(anyhow::anyhow!("attachment object already exists"))
        } else {
            Err(error.into())
        };
    }
    temp.disarm();
    created.disarm();

    #[cfg(test)]
    if FORCE_OBJECT_VERIFICATION_FAILURE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("attachment object verification failed");
    }
    let verified = object_metadata(&std::fs::read(&final_path)?);
    if verified != actual {
        anyhow::bail!("attachment object verification failed");
    }
    Ok(StoredObject {
        object_key,
        size_bytes: verified.size_bytes,
        checksum_sha256: verified.checksum_sha256,
    })
}

#[cfg(test)]
static FORCE_OBJECT_VERIFICATION_FAILURE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn validate_sha256(value: &str) -> anyhow::Result<String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        anyhow::bail!("invalid expected SHA-256 checksum");
    }
    Ok(value.to_string())
}

fn object_metadata(bytes: &[u8]) -> ObjectMetadata {
    ObjectMetadata {
        size_bytes: bytes.len() as u64,
        checksum_sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let source = std::ffi::CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "invalid source path"))?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "invalid destination path"))?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::hard_link(source, destination)?;
    std::fs::remove_file(source)
}

#[async_trait::async_trait]
impl PreparedAttachmentMutation for PreparedFilesystemPut {
    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        let prepared = *self;
        tokio::task::spawn_blocking(move || publish_put_blocking(prepared))
            .await
            .map_err(|error| anyhow::anyhow!("attachment put publication task failed: {error}"))?
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        let Self {
            note_dir: _,
            relative_path: _,
            mut staging,
            _coordination_guard,
        } = *self;
        staging.cleanup().await?;
        drop(_coordination_guard);
        Ok(())
    }
}

#[async_trait::async_trait]
impl PreparedAttachmentMutation for PreparedFilesystemDelete {
    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        let prepared = *self;
        tokio::task::spawn_blocking(move || publish_delete_blocking(prepared))
            .await
            .map_err(|error| {
                anyhow::anyhow!("attachment delete publication task failed: {error}")
            })?
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        Ok(())
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

fn publish_put_blocking(prepared: PreparedFilesystemPut) -> anyhow::Result<()> {
    let PreparedFilesystemPut {
        note_dir,
        relative_path,
        mut staging,
        _coordination_guard,
    } = prepared;
    let mut created_directories =
        ensure_safe_parent_directories_blocking(&note_dir, &relative_path)?;
    let final_path = attachment_path_on_disk(&note_dir, &relative_path);
    let backup_path = final_path.with_file_name(format!(
        ".{}-{}.backup",
        final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment"),
        uuid::Uuid::new_v4()
    ));
    let mut rollback = FilePublicationRollback::new(final_path.clone());

    match std::fs::symlink_metadata(&final_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("attachment path must not contain a symlink")
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!("attachment target must be a file")
        }
        Ok(_) => {
            std::fs::rename(&final_path, &backup_path)?;
            rollback.arm(backup_path);
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if let Err(promotion_error) = rename_put_path(
        staging.path(),
        &final_path,
        &final_path,
        PutFailurePhase::Promotion,
    ) {
        let restore_error = rollback.restore_blocking().err();
        return Err(put_publication_error(promotion_error, restore_error));
    }
    staging.disarm();
    created_directories.disarm();
    rollback.mark_replacement_selected();

    rollback.cleanup_obsolete_blocking()?;
    drop(_coordination_guard);
    Ok(())
}

fn publish_delete_blocking(prepared: PreparedFilesystemDelete) -> anyhow::Result<()> {
    let PreparedFilesystemDelete {
        note_dir,
        relative_path,
        final_path,
        _coordination_guard,
    } = prepared;
    if !existing_safe_directory_blocking(&note_dir)? {
        return Ok(());
    }
    reject_symlink_components_blocking(&note_dir, &relative_path)?;
    remove_file_if_present_blocking(&final_path)?;
    prune_empty_attachment_directories_blocking(final_path.parent(), &note_dir)?;
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

impl FileCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("armed file cleanup guard")
    }

    fn disarm(&mut self) {
        self.path = None;
    }

    async fn cleanup(&mut self) -> anyhow::Result<()> {
        if let Some(path) = self.path.as_deref() {
            remove_file_if_present(path).await?;
            self.disarm();
        }
        Ok(())
    }
}

impl Drop for FileCleanupGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl CreatedDirectoriesCleanupGuard {
    fn new() -> Self {
        Self { paths: Vec::new() }
    }

    fn record(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn disarm(&mut self) {
        self.paths.clear();
    }
}

impl Drop for CreatedDirectoriesCleanupGuard {
    fn drop(&mut self) {
        for path in self.paths.iter().rev() {
            let _ = std::fs::remove_dir(path);
        }
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

impl FilePublicationRollback {
    fn new(final_path: PathBuf) -> Self {
        Self {
            final_path,
            backup_path: None,
            replacement_selected: false,
        }
    }

    fn arm(&mut self, backup_path: PathBuf) {
        self.backup_path = Some(backup_path);
    }

    fn restore_blocking(&mut self) -> anyhow::Result<()> {
        if let Some(backup_path) = self.backup_path.as_deref() {
            rename_put_path(
                backup_path,
                &self.final_path,
                &self.final_path,
                PutFailurePhase::Restore,
            )?;
            self.backup_path = None;
        }
        Ok(())
    }

    fn mark_replacement_selected(&mut self) {
        self.replacement_selected = true;
    }

    fn cleanup_obsolete_blocking(&mut self) -> anyhow::Result<()> {
        if let Some(backup_path) = self.backup_path.as_deref() {
            fail_put_operation(&self.final_path, PutFailurePhase::BackupCleanup)?;
            remove_file_if_present_blocking(backup_path)?;
            self.backup_path = None;
        }
        Ok(())
    }
}

impl Drop for FilePublicationRollback {
    fn drop(&mut self) {
        let Some(backup_path) = self.backup_path.take() else {
            return;
        };
        if self.replacement_selected {
            let _ = std::fs::remove_file(backup_path);
        } else if !self.final_path.exists() {
            let _ = std::fs::rename(backup_path, &self.final_path);
        }
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

fn reject_symlink_components_blocking(
    note_dir: &Path,
    attachment_path: &str,
) -> anyhow::Result<()> {
    let mut path = note_dir.to_path_buf();
    for segment in attachment_path.split('/') {
        path.push(segment);
        match std::fs::symlink_metadata(&path) {
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

fn ensure_safe_parent_directories_blocking(
    note_dir: &Path,
    attachment_path: &str,
) -> anyhow::Result<CreatedDirectoriesCleanupGuard> {
    let mut created = CreatedDirectoriesCleanupGuard::new();
    match std::fs::symlink_metadata(note_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("attachment directory must not be a symlink")
        }
        Ok(metadata) if !metadata.is_dir() => {
            anyhow::bail!("attachment directory must be a directory")
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            std::fs::create_dir(note_dir)?;
            created.record(note_dir.to_path_buf());
        }
        Err(error) => return Err(error.into()),
    }

    let final_path = attachment_path_on_disk(note_dir, attachment_path);
    let Some(parent) = final_path.parent() else {
        return Ok(created);
    };
    let relative_parent = parent
        .strip_prefix(note_dir)
        .expect("attachment parent must remain below note directory");
    let mut current = note_dir.to_path_buf();
    for component in relative_parent.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("attachment path must not contain a symlink")
            }
            Ok(metadata) if !metadata.is_dir() => {
                anyhow::bail!("attachment parent must be a directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
                created.record(current.clone());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(created)
}

async fn remove_directory_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn remove_file_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_file_if_present_blocking(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn prune_empty_attachment_directories_blocking(
    mut directory: Option<&Path>,
    note_dir: &Path,
) -> anyhow::Result<()> {
    while let Some(path) = directory {
        if !path.starts_with(note_dir) {
            break;
        }
        match std::fs::remove_dir(path) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                ) =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
        if path == note_dir {
            break;
        }
        directory = path.parent();
    }
    Ok(())
}

fn remove_directory_if_present_blocking(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn put_publication_error(
    promotion_error: std::io::Error,
    restore_error: Option<anyhow::Error>,
) -> anyhow::Error {
    let mut message = format!("failed to promote staged attachment: {promotion_error}");
    if let Some(error) = restore_error {
        message.push_str(&format!("; failed to restore previous attachment: {error}"));
    }
    anyhow::anyhow!(message)
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

fn rename_put_path(
    from: &Path,
    to: &Path,
    final_path: &Path,
    phase: PutFailurePhase,
) -> std::io::Result<()> {
    fail_put_operation(final_path, phase)?;
    std::fs::rename(from, to)
}

fn fail_put_operation(final_path: &Path, phase: PutFailurePhase) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let mut failures = put_failures()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = failures
            .iter()
            .position(|failure| failure.final_path == final_path && failure.phase == phase)
        {
            failures.swap_remove(index);
            return Err(std::io::Error::other("injected attachment put failure"));
        }
    }
    #[cfg(not(test))]
    let _ = (final_path, phase);
    Ok(())
}

#[cfg(test)]
struct PutFailure {
    final_path: PathBuf,
    phase: PutFailurePhase,
}

#[cfg(test)]
fn put_failures() -> &'static StdMutex<Vec<PutFailure>> {
    static FAILURES: std::sync::OnceLock<StdMutex<Vec<PutFailure>>> = std::sync::OnceLock::new();
    FAILURES.get_or_init(|| StdMutex::new(Vec::new()))
}

#[cfg(test)]
fn install_put_failure(final_path: PathBuf, phase: PutFailurePhase) {
    put_failures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(PutFailure { final_path, phase });
}

#[cfg(test)]
struct PreparePutPause {
    note_dir: PathBuf,
    reached: tokio::sync::oneshot::Sender<PathBuf>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
fn prepare_put_pauses() -> &'static StdMutex<Vec<PreparePutPause>> {
    static PAUSES: std::sync::OnceLock<StdMutex<Vec<PreparePutPause>>> = std::sync::OnceLock::new();
    PAUSES.get_or_init(|| StdMutex::new(Vec::new()))
}

#[cfg(test)]
fn install_prepare_put_pause(
    note_dir: PathBuf,
) -> (
    tokio::sync::oneshot::Receiver<PathBuf>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (reached_sender, reached_receiver) = tokio::sync::oneshot::channel();
    let (resume_sender, resume_receiver) = tokio::sync::oneshot::channel();
    prepare_put_pauses()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(PreparePutPause {
            note_dir,
            reached: reached_sender,
            resume: resume_receiver,
        });
    (reached_receiver, resume_sender)
}

#[cfg(test)]
async fn pause_prepare_put(note_dir: &Path, staging_path: &Path) {
    let pause = {
        let mut pauses = prepare_put_pauses()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pauses
            .iter()
            .position(|pause| pause.note_dir == note_dir)
            .map(|index| pauses.swap_remove(index))
    };
    if let Some(pause) = pause {
        let _ = pause.reached.send(staging_path.to_path_buf());
        let _ = pause.resume.await;
    }
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

    fn assert_only_final_file(final_dir: &Path, final_path: &Path) {
        let entries = std::fs::read_dir(final_dir)
            .expect("read note attachment directory")
            .map(|entry| entry.expect("read attachment entry").path())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![final_path]);
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

    #[tokio::test]
    async fn failed_restore_is_retried_by_put_rollback_guard() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        let final_dir = root.join("note-1");
        let final_path = final_dir.join("file.txt");
        let store = FilesystemAttachmentStore::new(root.clone());
        store
            .prepare("note-1", &[attachment(b"old")])
            .await
            .unwrap()
            .publish()
            .await
            .unwrap();
        let prepared = store
            .prepare_put("note-1", &attachment(b"new"))
            .await
            .unwrap();
        install_put_failure(final_path.clone(), PutFailurePhase::Promotion);
        install_put_failure(final_path.clone(), PutFailurePhase::Restore);

        let error = prepared
            .publish()
            .await
            .expect_err("injected promotion and first restore must fail");

        assert!(error
            .to_string()
            .contains("failed to restore previous attachment"));
        assert_eq!(store.read("note-1", "./file.txt").await.unwrap(), b"old");
        assert_only_final_directory(&root, &final_dir);
        assert_only_final_file(&final_dir, &final_path);
    }

    #[tokio::test]
    async fn failed_backup_cleanup_is_retried_by_put_rollback_guard() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        let final_dir = root.join("note-1");
        let final_path = final_dir.join("file.txt");
        let store = FilesystemAttachmentStore::new(root.clone());
        store
            .prepare("note-1", &[attachment(b"old")])
            .await
            .unwrap()
            .publish()
            .await
            .unwrap();
        let prepared = store
            .prepare_put("note-1", &attachment(b"new"))
            .await
            .unwrap();
        install_put_failure(final_path.clone(), PutFailurePhase::BackupCleanup);

        prepared
            .publish()
            .await
            .expect_err("injected first backup cleanup must fail");

        assert_eq!(store.read("note-1", "./file.txt").await.unwrap(), b"new");
        assert_only_final_directory(&root, &final_dir);
        assert_only_final_file(&final_dir, &final_path);
    }

    #[tokio::test]
    async fn cancelling_paused_prepare_put_cleans_staging_and_releases_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        let note_dir = root.join("note-1");
        let store = FilesystemAttachmentStore::new(root.clone());
        let (reached, _resume) = install_prepare_put_pause(note_dir);
        let prepare_store = store.clone();
        let prepare = tokio::spawn(async move {
            prepare_store
                .prepare_put("note-1", &attachment(b"new"))
                .await
        });

        let staging_path = tokio::time::timeout(Duration::from_secs(2), reached)
            .await
            .expect("prepare put must reach deterministic staging pause")
            .expect("prepare put pause sender");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        let blocked_store = store.clone();
        let mut blocked = tokio::spawn(async move {
            blocked_store
                .prepare_put("note-1", &attachment(b"replacement"))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut blocked)
                .await
                .is_err()
        );

        prepare.abort();
        let join_error = match prepare.await {
            Err(error) => error,
            Ok(_) => panic!("paused preparation task must be cancelled"),
        };
        assert!(join_error.is_cancelled());
        assert!(!staging_path.exists());

        let replacement = tokio::time::timeout(Duration::from_secs(2), blocked)
            .await
            .expect("cancelled preparation must release note lock")
            .unwrap()
            .unwrap();
        replacement.abort().await.unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn verification_failure_leaves_published_object_as_safe_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        let store = FilesystemAttachmentStore::new(root.clone());
        FORCE_OBJECT_VERIFICATION_FAILURE.store(true, std::sync::atomic::Ordering::SeqCst);
        let error = store
            .put_immutable(PutObjectRequest {
                object_key: "objects/orphan".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
                    .into(),
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("verification failed"));
        assert_eq!(
            std::fs::read(root.join("objects/orphan")).unwrap(),
            b"payload"
        );
    }
}
