use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Hex-encoded BLAKE3 content identity.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct BlobHash(String);

impl BlobHash {
    /// Hash bytes without writing them.
    #[must_use]
    pub fn digest(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }

    /// Parse and validate a stored digest.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let valid = value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        valid.then(|| Self(value.to_ascii_lowercase()))
    }

    /// Lowercase hexadecimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BlobHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("BlobHash").field(&self.0).finish()
    }
}

/// Content-addressed directory with atomic writes.
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Open a blob store and create its root when missing.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors raised while creating the root.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Persist bytes if their content is not already present.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors raised during creation, writing, or atomic rename.
    pub fn put(&self, bytes: &[u8]) -> io::Result<BlobHash> {
        let hash = BlobHash::digest(bytes);
        let destination = self.path(&hash);
        if destination.is_file() {
            return Ok(hash);
        }

        let temporary =
            self.root
                .join(format!(".{}.{}.partial", hash.as_str(), std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let write_result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            match fs::rename(&temporary, &destination) {
                Ok(()) => Ok(()),
                Err(error) if destination.is_file() => {
                    fs::remove_file(&temporary)?;
                    let _ = error;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;
        Ok(hash)
    }

    /// Resolve a validated content identity beneath the store root.
    #[must_use]
    pub fn path(&self, hash: &BlobHash) -> PathBuf {
        self.root.join(hash.as_str())
    }

    /// Read and verify a blob.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is unavailable or does not match its identity.
    pub fn read(&self, hash: &BlobHash) -> io::Result<Vec<u8>> {
        let bytes = fs::read(self.path(hash))?;
        if BlobHash::digest(&bytes) != *hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "blob hash mismatch",
            ));
        }
        Ok(bytes)
    }

    /// Root directory used by this store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_deduplicates_and_read_verifies() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = BlobStore::open(directory.path()).expect("open blob store");
        let first = store.put(b"hello").expect("put blob");
        let second = store.put(b"hello").expect("deduplicate blob");
        assert_eq!(first, second);
        assert_eq!(store.read(&first).expect("read blob"), b"hello");
        assert_eq!(
            fs::read_dir(directory.path()).expect("list blobs").count(),
            1
        );
    }

    #[test]
    fn parse_rejects_path_material() {
        assert!(BlobHash::parse("../escape").is_none());
        assert!(BlobHash::parse(&"a".repeat(64)).is_some());
    }
}
