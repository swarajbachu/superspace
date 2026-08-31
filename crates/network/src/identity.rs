use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use thiserror::Error;
use uuid::Uuid;

use crate::{DeviceKeypair, PairingError, TransportError, TransportIdentity};

const STORE_VERSION: u16 = 1;
const MAX_IDENTITY_BYTES: u64 = 64 * 1024;
const PRIVATE_MODE: u32 = 0o600;

/// Stable local identity shared by discovery, Noise pairing, and pinned QUIC transport.
#[derive(Clone, Eq, PartialEq)]
pub struct LocalIdentity {
    /// Random installation identifier, independent from host names and network addresses.
    pub device_id: Uuid,
    /// Long-lived Noise XX static keypair.
    pub noise: DeviceKeypair,
    /// Long-lived self-signed TLS identity pinned during pairing.
    pub transport: TransportIdentity,
}

impl LocalIdentity {
    /// Generate unrelated identifiers and keys from operating-system randomness.
    ///
    /// # Errors
    /// Returns a cryptographic provider failure.
    pub fn generate() -> Result<Self, IdentityStoreError> {
        Ok(Self {
            device_id: Uuid::new_v4(),
            noise: DeviceKeypair::generate()?,
            transport: TransportIdentity::generate()?,
        })
    }

    /// Load a stable identity or atomically create an owner-only file on first run.
    ///
    /// Existing files with group/world permissions, symlinks, oversized payloads, malformed keys,
    /// or mismatched certificates are rejected instead of silently rotating peer trust.
    ///
    /// # Errors
    /// Returns an I/O, permission, encoding, version, or key-validation failure.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self, IdentityStoreError> {
        let path = path.as_ref();
        match Self::load(path) {
            Ok(identity) => return Ok(identity),
            Err(IdentityStoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let identity = Self::generate()?;
        let wire = IdentityWire::from(&identity);
        let mut bytes = Vec::new();
        ciborium::into_writer(&wire, &mut bytes).map_err(|_| IdentityStoreError::Codec)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IDENTITY_BYTES {
            return Err(IdentityStoreError::Oversized);
        }
        let temporary = temporary_path(path);
        let mut file = create_private(&temporary)?;
        if let Err(error) = (|| -> std::io::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()
        })() {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        drop(file);
        match fs::hard_link(&temporary, path) {
            Ok(()) => {
                fs::remove_file(&temporary)?;
                Ok(identity)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary)?;
                Self::load(path)
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error.into())
            }
        }
    }

    fn load(path: &Path) -> Result<Self, IdentityStoreError> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() {
            return Err(IdentityStoreError::UnsafeFile);
        }
        validate_private_permissions(&metadata)?;
        if metadata.len() > MAX_IDENTITY_BYTES {
            return Err(IdentityStoreError::Oversized);
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        File::open(path)?
            .take(MAX_IDENTITY_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IDENTITY_BYTES {
            return Err(IdentityStoreError::Oversized);
        }
        let wire: IdentityWire =
            ciborium::from_reader(bytes.as_slice()).map_err(|_| IdentityStoreError::Codec)?;
        wire.try_into()
    }
}

impl fmt::Debug for LocalIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalIdentity")
            .field("device_id", &self.device_id)
            .field("noise", &self.noise)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
struct IdentityWire {
    version: u16,
    device_id: Uuid,
    #[serde(with = "serde_bytes")]
    noise_private: Vec<u8>,
    #[serde(with = "serde_bytes")]
    noise_public: Vec<u8>,
    #[serde(with = "serde_bytes")]
    certificate: Vec<u8>,
    #[serde(with = "serde_bytes")]
    transport_private: Vec<u8>,
}

impl From<&LocalIdentity> for IdentityWire {
    fn from(identity: &LocalIdentity) -> Self {
        Self {
            version: STORE_VERSION,
            device_id: identity.device_id,
            noise_private: identity.noise.private_key().to_vec(),
            noise_public: identity.noise.public_key().to_vec(),
            certificate: identity.transport.certificate_der().to_vec(),
            transport_private: identity.transport.private_key_der().to_vec(),
        }
    }
}

impl TryFrom<IdentityWire> for LocalIdentity {
    type Error = IdentityStoreError;

    fn try_from(wire: IdentityWire) -> Result<Self, Self::Error> {
        if wire.version != STORE_VERSION {
            return Err(IdentityStoreError::UnsupportedVersion(wire.version));
        }
        if wire.device_id.is_nil() {
            return Err(IdentityStoreError::InvalidDeviceId);
        }
        Ok(Self {
            device_id: wire.device_id,
            noise: DeviceKeypair::from_bytes(wire.noise_private, wire.noise_public)?,
            transport: TransportIdentity::from_der(wire.certificate, wire.transport_private)?,
        })
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{}.tmp", Uuid::new_v4()))
}

fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(PRIVATE_MODE);
    options.open(path)
}

#[cfg(unix)]
fn validate_private_permissions(metadata: &fs::Metadata) -> Result<(), IdentityStoreError> {
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(IdentityStoreError::UnsafePermissions);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_metadata: &fs::Metadata) -> Result<(), IdentityStoreError> {
    Ok(())
}

/// Stable identity persistence failures.
#[derive(Debug, Error)]
pub enum IdentityStoreError {
    /// Filesystem operation failed.
    #[error("local identity filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// CBOR payload was malformed.
    #[error("local identity payload is malformed")]
    Codec,
    /// File exceeded the strict private-identity size bound.
    #[error("local identity payload is oversized")]
    Oversized,
    /// File was a symlink, directory, or another unsafe file type.
    #[error("local identity path is not a regular file")]
    UnsafeFile,
    /// Group or other users can access private key material.
    #[error("local identity file permissions are not private")]
    UnsafePermissions,
    /// Identity was created by an unsupported newer format.
    #[error("unsupported local identity version {0}")]
    UnsupportedVersion(u16),
    /// Installation UUID was nil.
    #[error("local device ID is invalid")]
    InvalidDeviceId,
    /// Noise identity was malformed.
    #[error("local Noise identity is invalid")]
    Pairing(#[from] PairingError),
    /// TLS certificate or private key was malformed or mismatched.
    #[error("local transport identity is invalid")]
    Transport(#[from] TransportError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_private_and_redacted() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("identity.cbor");
        let created = LocalIdentity::load_or_create(&path).expect("create");
        let loaded = LocalIdentity::load_or_create(&path).expect("load");
        assert_eq!(created, loaded);
        let debug = format!("{loaded:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&hex(&loaded.noise.private_key()[..8])));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
            PRIVATE_MODE
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_permissions_and_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("identity.cbor");
        LocalIdentity::load_or_create(&path).expect("create");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("permissions");
        assert!(matches!(
            LocalIdentity::load_or_create(&path),
            Err(IdentityStoreError::UnsafePermissions)
        ));
        let link = directory.path().join("identity-link.cbor");
        symlink(&path, &link).expect("symlink");
        assert!(matches!(
            LocalIdentity::load_or_create(link),
            Err(IdentityStoreError::UnsafeFile)
        ));
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut value, byte| {
            use std::fmt::Write as _;
            let _ = write!(value, "{byte:02x}");
            value
        })
    }
}
