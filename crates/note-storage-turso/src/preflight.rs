use note_storage::{StorageError, StorageErrorKind, StorageResult};
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;
use turso::core::io::FileId;
use turso::core::{
    Buffer, Clock, Completion, CompletionError, File, MonotonicInstant, OpenFlags, PlatformIO,
    WallClockInstant, IO,
};

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const SQLITE_HEADER_LEN: usize = 100;
pub(crate) const APPLICATION_ID: u32 = 0x414E4F54;
pub(crate) const OLDEST_SCHEMA_VERSION: u32 = 2;
pub(crate) const PREVIOUS_SCHEMA_VERSION: u32 = 3;
pub(crate) const SCHEMA_VERSION: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Preflight {
    Missing,
    Fresh,
    Existing { version: u32 },
}

/// Keeps the validated main-file handle stable while Turso opens its sidecars.
pub(crate) struct PinnedIo {
    inner: PlatformIO,
    main_path: String,
    main_file: Arc<dyn File>,
    main_file_id: FileId,
}

impl PinnedIo {
    pub(crate) fn prepare(
        path: &Path,
        path_string: &str,
        eligible: Preflight,
    ) -> StorageResult<(Arc<Self>, Preflight)> {
        let inner = PlatformIO::new()
            .map_err(|error| turso_io_error(path, "initialize database I/O", error))?;
        let before_id = inner
            .file_id(path_string)
            .map_err(|error| turso_io_error(path, "identify database file", error))?;
        let main_file = inner
            .open_file(path_string, OpenFlags::None, true)
            .map_err(|error| turso_io_error(path, "pin database file", error))?;
        let after_id = inner
            .file_id(path_string)
            .map_err(|error| turso_io_error(path, "identify pinned database file", error))?;

        if before_id != after_id {
            return Err(path_changed(path));
        }

        let io = Arc::new(Self {
            inner,
            main_path: path_string.to_owned(),
            main_file,
            main_file_id: before_id,
        });
        let actual = io.pinned_state(path)?;
        match (eligible, actual) {
            (Preflight::Fresh, Preflight::Fresh | Preflight::Existing { .. })
            | (Preflight::Existing { .. }, Preflight::Existing { .. }) => Ok((io, actual)),
            _ => Err(incompatible_database(path)),
        }
    }

    pub(crate) fn verify_ready(&self, path: &Path) -> StorageResult<()> {
        let current_id = self
            .inner
            .file_id(&self.main_path)
            .map_err(|error| turso_io_error(path, "identify opened database file", error))?;
        if current_id != self.main_file_id {
            return Err(path_changed(path));
        }
        match self.pinned_state(path)? {
            Preflight::Existing {
                version: SCHEMA_VERSION,
            } => Ok(()),
            Preflight::Existing { version } => Err(unsupported_schema(version)),
            Preflight::Missing | Preflight::Fresh => Err(incompatible_database(path)),
        }
    }

    fn pinned_state(&self, path: &Path) -> StorageResult<Preflight> {
        let size = self
            .main_file
            .size()
            .map_err(|error| turso_io_error(path, "inspect pinned database file", error))?;
        if size == 0 {
            return Ok(Preflight::Fresh);
        }
        if size < SQLITE_HEADER_LEN as u64 {
            return Err(corrupt_database(path));
        }

        let header = Arc::new(Buffer::new(vec![0; SQLITE_HEADER_LEN]));
        let completion = Completion::new_read(header.clone(), |result| {
            let Ok((_, bytes_read)) = result else {
                return None;
            };
            let actual = usize::try_from(bytes_read).unwrap_or_default();
            (actual != SQLITE_HEADER_LEN).then_some(CompletionError::ShortRead {
                page_idx: 1,
                expected: SQLITE_HEADER_LEN,
                actual,
            })
        });
        let completion = self
            .main_file
            .pread(0, completion)
            .map_err(|error| turso_io_error(path, "read pinned database header", error))?;
        self.inner
            .wait_for_completion(completion)
            .map_err(|error| turso_io_error(path, "read pinned database header", error))?;

        classify_header(path, header.as_slice())
    }
}

impl Clock for PinnedIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }

    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}

impl IO for PinnedIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso::core::Result<Arc<dyn File>> {
        if direct && path == self.main_path {
            Ok(self.main_file.clone())
        } else {
            self.inner.open_file(path, flags, direct)
        }
    }

    fn remove_file(&self, path: &str) -> turso::core::Result<()> {
        self.inner.remove_file(path)
    }

    fn supports_shared_wal_coordination(&self) -> bool {
        self.inner.supports_shared_wal_coordination()
    }

    fn step(&self) -> turso::core::Result<()> {
        self.inner.step()
    }

    fn file_id(&self, path: &str) -> turso::core::Result<FileId> {
        if path == self.main_path {
            Ok(self.main_file_id)
        } else {
            self.inner.file_id(path)
        }
    }
}

pub(crate) fn preflight(path: &Path) -> StorageResult<Preflight> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match std::fs::symlink_metadata(path) {
                Ok(_) => Err(io_error(path, error)),
                Err(symlink_error) if symlink_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(Preflight::Missing)
                }
                Err(symlink_error) => Err(io_error(path, symlink_error)),
            };
        }
        Err(error) => return Err(io_error(path, error)),
        Ok(metadata) if metadata.len() == 0 => return Ok(Preflight::Fresh),
        Ok(_) => {}
    }

    let mut header = [0_u8; SQLITE_HEADER_LEN];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| header_error(path, error))?;

    classify_header(path, &header)
}

fn classify_header(path: &Path, header: &[u8]) -> StorageResult<Preflight> {
    if &header[0..16] != SQLITE_HEADER {
        return Err(corrupt_database(path));
    }

    let user_version = u32::from_be_bytes(header[60..64].try_into().unwrap());
    let application_id = u32::from_be_bytes(header[68..72].try_into().unwrap());
    if application_id != APPLICATION_ID {
        return Err(incompatible_database(path));
    }
    if !matches!(
        user_version,
        OLDEST_SCHEMA_VERSION | PREVIOUS_SCHEMA_VERSION | SCHEMA_VERSION
    ) {
        return Err(unsupported_schema(user_version));
    }

    Ok(Preflight::Existing {
        version: user_version,
    })
}

pub(crate) fn preflight_and_reserve(path: &Path) -> StorageResult<Preflight> {
    loop {
        match preflight(path)? {
            Preflight::Missing => {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    Ok(_) => return Ok(Preflight::Fresh),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(io_error(path, error)),
                }
            }
            state => return Ok(state),
        }
    }
}

pub(crate) fn incompatible_database(path: &Path) -> StorageError {
    StorageError::new(
        StorageErrorKind::IncompatibleDatabase,
        format!(
            "{} is not an agent-note database; remove the legacy test database and restart",
            path.display()
        ),
    )
}

pub(crate) fn unsupported_schema(user_version: u32) -> StorageError {
    StorageError::new(
        StorageErrorKind::UnsupportedSchema,
        format!("unsupported agent-note schema version {user_version}"),
    )
}

fn io_error(path: &Path, error: std::io::Error) -> StorageError {
    StorageError::with_source(
        StorageErrorKind::Unavailable,
        format!("cannot inspect database {}", path.display()),
        error,
    )
}

fn header_error(path: &Path, error: std::io::Error) -> StorageError {
    let kind = if error.kind() == std::io::ErrorKind::UnexpectedEof {
        StorageErrorKind::Corrupt
    } else {
        StorageErrorKind::Unavailable
    };
    StorageError::with_source(
        kind,
        format!("cannot read SQLite header from {}", path.display()),
        error,
    )
}

fn corrupt_database(path: &Path) -> StorageError {
    StorageError::new(
        StorageErrorKind::Corrupt,
        format!("{} is not a valid SQLite database", path.display()),
    )
}

fn path_changed(path: &Path) -> StorageError {
    StorageError::new(
        StorageErrorKind::Conflict,
        format!(
            "database path changed while it was being opened: {}",
            path.display()
        ),
    )
}

fn turso_io_error(path: &Path, operation: &str, error: turso::core::LimboError) -> StorageError {
    let kind = match error {
        turso::core::LimboError::Busy
        | turso::core::LimboError::BusySnapshot
        | turso::core::LimboError::LockingError(_) => StorageErrorKind::Conflict,
        turso::core::LimboError::Corrupt(_)
        | turso::core::LimboError::CompletionError(CompletionError::ShortRead { .. }) => {
            StorageErrorKind::Corrupt
        }
        _ => StorageErrorKind::Unavailable,
    };
    StorageError::with_source(kind, format!("{operation}: {}", path.display()), error)
}

#[cfg(test)]
mod tests {
    use super::preflight;
    use note_storage::StorageErrorKind;

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_not_treated_as_a_missing_database() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        symlink(dir.path().join("missing-target.db"), &path).unwrap();

        let error = preflight(&path).unwrap_err();

        assert_eq!(error.kind(), StorageErrorKind::Unavailable);
    }
}
