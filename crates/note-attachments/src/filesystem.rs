use crate::{
    path::{canonical_object_key, canonical_relative_path, note_directory},
    AttachmentStore, AttachmentStoreInfo, DeleteObjectOutcome, ObjectMetadata, PutObjectRequest,
    StoredObject,
};
use sha2::{Digest, Sha256};
use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};
use tokio::fs;

/// Filesystem-backed attachment storage.
///
/// The configured root must be exclusively owned by this application. Callers
/// must not permit external mutation or creation of symlinks below the root.
#[derive(Clone)]
pub struct FilesystemAttachmentStore {
    root: PathBuf,
}

impl FilesystemAttachmentStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
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
        delete_file(&attachment_path_on_disk(&self.root, &object_key)).await
    }

    async fn delete_legacy(
        &self,
        note_id: &str,
        user_path: &str,
    ) -> anyhow::Result<DeleteObjectOutcome> {
        let note_dir = note_directory(&self.root, note_id)?;
        let relative_path = canonical_relative_path(user_path)?;
        if !existing_safe_directory(&note_dir).await? {
            return Ok(DeleteObjectOutcome::AlreadyAbsent);
        }
        reject_symlink_components(&note_dir, &relative_path).await?;
        delete_file(&attachment_path_on_disk(&note_dir, &relative_path)).await
    }

    async fn read_legacy(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        let note_dir = note_directory(&self.root, note_id)?;
        if !existing_safe_directory(&note_dir).await? {
            anyhow::bail!("attachment note directory does not exist");
        }
        let relative_path = canonical_relative_path(path)?;
        reject_symlink_components(&note_dir, &relative_path).await?;
        Ok(fs::read(attachment_path_on_disk(&note_dir, &relative_path)).await?)
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "filesystem".to_string(),
            location: Some(self.root.to_string_lossy().into_owned()),
        }
    }
}

async fn delete_file(path: &Path) -> anyhow::Result<DeleteObjectOutcome> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(DeleteObjectOutcome::Deleted),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(DeleteObjectOutcome::AlreadyAbsent),
        Err(error) => Err(error.into()),
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
    #[cfg(test)]
    if FORCE_OBJECT_FILE_SYNC_FAILURE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("attachment object file sync failed");
    }
    file.sync_all()?;
    drop(file);

    if let Err(error) = rename_noreplace(temp.path(), &final_path) {
        return if error.kind() == ErrorKind::AlreadyExists {
            Err(anyhow::anyhow!("attachment object already exists"))
        } else {
            Err(error.into())
        };
    }
    temp.disarm();
    let created_directories = created.disarm();

    #[cfg(test)]
    if FORCE_OBJECT_DIRECTORY_SYNC_FAILURE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("attachment object directory sync failed");
    }
    sync_directory(
        final_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("attachment object has no parent directory"))?,
    )?;
    for directory in created_directories.iter().rev() {
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
    }

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
#[cfg(test)]
static FORCE_OBJECT_FILE_SYNC_FAILURE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static FORCE_OBJECT_DIRECTORY_SYNC_FAILURE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

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

struct FileCleanupGuard {
    path: Option<PathBuf>,
}

impl FileCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("cleanup guard is armed")
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for FileCleanupGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct CreatedDirectoriesCleanupGuard {
    paths: Vec<PathBuf>,
}

impl CreatedDirectoriesCleanupGuard {
    fn disarm(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.paths)
    }
}

impl Drop for CreatedDirectoriesCleanupGuard {
    fn drop(&mut self) {
        for path in self.paths.iter().rev() {
            let _ = std::fs::remove_dir(path);
        }
    }
}

async fn existing_safe_directory(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("attachment directory must not be a symlink")
        }
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => anyhow::bail!("attachment directory path is not a directory"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn attachment_path_on_disk(root: &Path, relative_path: &str) -> PathBuf {
    relative_path
        .split('/')
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

async fn reject_symlink_components(root: &Path, relative_path: &str) -> anyhow::Result<()> {
    let mut current = root.to_path_buf();
    for component in relative_path.split('/') {
        current.push(component);
        match fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("attachment path must not contain symlinks")
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn reject_symlink_components_blocking(root: &Path, relative_path: &str) -> anyhow::Result<()> {
    let mut current = root.to_path_buf();
    for component in relative_path.split('/') {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("attachment path must not contain symlinks")
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_safe_parent_directories_blocking(
    root: &Path,
    relative_path: &str,
) -> anyhow::Result<CreatedDirectoriesCleanupGuard> {
    let mut created = Vec::new();
    if !validate_configured_root_blocking(root)? {
        std::fs::create_dir_all(root)?;
        created.push(root.to_path_buf());
    }
    reject_symlink_components_blocking(root, relative_path)?;
    let final_path = attachment_path_on_disk(root, relative_path);
    let mut current = root.to_path_buf();
    if let Some(parent) = final_path.parent() {
        for component in parent.strip_prefix(root)?.components() {
            current.push(component);
            match std::fs::create_dir(&current) {
                Ok(()) => created.push(current.clone()),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    let metadata = std::fs::symlink_metadata(&current)?;
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        anyhow::bail!("attachment path contains an unsafe directory")
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(CreatedDirectoriesCleanupGuard { paths: created })
}

fn validate_configured_root_blocking(root: &Path) -> anyhow::Result<bool> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("attachment root must not be a symlink")
        }
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => anyhow::bail!("attachment root path is not a directory"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FAULT_INJECTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn verification_failure_reports_a_safe_orphan_and_leaves_no_staging_file() {
        let _lock = FAULT_INJECTION_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        FORCE_OBJECT_VERIFICATION_FAILURE.store(true, std::sync::atomic::Ordering::SeqCst);

        let error = put_immutable_blocking(
            &root,
            PutObjectRequest {
                object_key: "objects/orphan".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
                    .into(),
            },
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "attachment object verification failed");
        assert_eq!(
            std::fs::read(root.join("objects/orphan")).unwrap(),
            b"payload"
        );
        assert_eq!(std::fs::read_dir(root.join("objects")).unwrap().count(), 1);
    }

    #[test]
    fn file_sync_failure_does_not_publish_the_object() {
        let _lock = FAULT_INJECTION_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        FORCE_OBJECT_FILE_SYNC_FAILURE.store(true, std::sync::atomic::Ordering::SeqCst);

        let error = put_immutable_blocking(
            &root,
            PutObjectRequest {
                object_key: "objects/not-published".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
                    .into(),
            },
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "attachment object file sync failed");
        assert!(!root.join("objects/not-published").exists());
    }

    #[test]
    fn directory_sync_failure_reports_a_safe_orphan_after_publication() {
        let _lock = FAULT_INJECTION_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        FORCE_OBJECT_DIRECTORY_SYNC_FAILURE.store(true, std::sync::atomic::Ordering::SeqCst);

        let error = put_immutable_blocking(
            &root,
            PutObjectRequest {
                object_key: "objects/orphan-after-sync".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
                    .into(),
            },
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "attachment object directory sync failed");
        assert_eq!(
            std::fs::read(root.join("objects/orphan-after-sync")).unwrap(),
            b"payload"
        );
        assert_eq!(std::fs::read_dir(root.join("objects")).unwrap().count(), 1);
    }
}
