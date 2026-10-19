use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Temporary files cap memory during network assembly. The backend API performs a
/// bounded final read because the current attachment store accepts byte objects.
pub struct StagedBlob {
    _temp: tempfile::NamedTempFile,
    file: tokio::fs::File,
    pub size: u64,
    received: u64,
    expected_hash: String,
    hasher: Sha256,
    verified: bool,
}
impl StagedBlob {
    pub fn new(size: u64, checksum: &str, limit: u64) -> Result<Self> {
        anyhow::ensure!(size <= limit, "sync blob exceeds negotiated limit");
        anyhow::ensure!(
            checksum.len() == 64
                && checksum
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid sync SHA-256"
        );
        let temp = tempfile::NamedTempFile::new().context("cannot stage sync blob")?;
        let file = tokio::fs::File::from_std(temp.reopen()?);
        Ok(Self {
            _temp: temp,
            file,
            size,
            received: 0,
            expected_hash: checksum.into(),
            hasher: Sha256::new(),
            verified: false,
        })
    }
    pub async fn append(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        anyhow::ensure!(
            !self.verified && offset == self.received,
            "sync chunk offset mismatch"
        );
        anyhow::ensure!(
            self.received
                .checked_add(bytes.len() as u64)
                .is_some_and(|n| n <= self.size),
            "sync blob exceeds declared size"
        );
        self.file
            .write_all(bytes)
            .await
            .context("cannot write staged sync blob")?;
        self.hasher.update(bytes);
        self.received += bytes.len() as u64;
        Ok(())
    }
    pub async fn finish(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.verified && self.received == self.size,
            "sync blob is incomplete"
        );
        anyhow::ensure!(
            format!("{:x}", self.hasher.clone().finalize()) == self.expected_hash,
            "sync blob SHA-256 mismatch"
        );
        self.file.flush().await?;
        self.verified = true;
        Ok(())
    }
    pub fn is_verified(&self) -> bool {
        self.verified
    }
    pub async fn read(&self) -> Result<Vec<u8>> {
        anyhow::ensure!(self.verified, "sync blob has not been verified");
        let mut reader = tokio::fs::File::from_std(self._temp.reopen()?);
        reader.seek(std::io::SeekFrom::Start(0)).await?;
        let mut bytes = Vec::new();
        reader.take(self.size + 1).read_to_end(&mut bytes).await?;
        anyhow::ensure!(bytes.len() as u64 == self.size, "staged sync blob changed");
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[tokio::test]
    async fn staged_blob_checks_offsets_limit_and_hash_before_reading() {
        let content = b"complete snapshot";
        let checksum = format!("{:x}", Sha256::digest(content));
        let mut blob = StagedBlob::new(content.len() as u64, &checksum, 100).unwrap();
        assert!(blob.append(1, b"x").await.is_err());
        blob.append(0, &content[..8]).await.unwrap();
        assert!(blob.append(8, &[0; 101]).await.is_err());
        blob.append(8, &content[8..]).await.unwrap();
        blob.finish().await.unwrap();
        assert_eq!(blob.read().await.unwrap(), content);
        let mut corrupt = StagedBlob::new(1, &checksum, 100).unwrap();
        corrupt.append(0, b"x").await.unwrap();
        assert!(corrupt.finish().await.is_err());
        assert!(StagedBlob::new(101, &checksum, 100).is_err());
    }
}

/// Prevent JSON escaping from multiplying an oversized snapshot allocation.
pub fn encode_metadata(event: &note_core::sync::SyncEvent, limit: u64) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: u64,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            if (self.bytes.len() as u64)
                .checked_add(input.len() as u64)
                .is_none_or(|n| n > self.limit)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "sync metadata exceeds negotiated limit",
                ));
            }
            self.bytes
                .try_reserve_exact(input.len())
                .map_err(std::io::Error::other)?;
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, event)?;
    Ok(writer.bytes)
}
