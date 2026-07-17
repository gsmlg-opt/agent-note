use note_storage::{StorageError, StorageErrorKind, StorageResult};
use std::io::Read as _;
use std::path::Path;

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
pub(crate) const APPLICATION_ID: u32 = 0x414E4F54;
pub(crate) const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Preflight {
    Missing,
    Fresh,
    Existing,
}

pub(crate) fn preflight(path: &Path) -> StorageResult<Preflight> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preflight::Missing);
        }
        Err(error) => return Err(io_error(path, error)),
        Ok(metadata) if metadata.len() == 0 => return Ok(Preflight::Fresh),
        Ok(_) => {}
    }

    let mut header = [0_u8; 100];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| header_error(path, error))?;

    if &header[0..16] != SQLITE_HEADER {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("{} is not a valid SQLite database", path.display()),
        ));
    }

    let user_version = u32::from_be_bytes(header[60..64].try_into().unwrap());
    let application_id = u32::from_be_bytes(header[68..72].try_into().unwrap());
    if application_id != APPLICATION_ID {
        return Err(incompatible_database(path));
    }
    if user_version != SCHEMA_VERSION {
        return Err(unsupported_schema(user_version));
    }

    Ok(Preflight::Existing)
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
