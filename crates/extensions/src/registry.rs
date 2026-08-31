use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};

use crate::{ExtensionPackage, PackageError};

const SCHEMA: u16 = 1;
const MAX_PACKAGE_BYTES: usize = 66 * 1024 * 1024;

/// A local Ed25519 identity used to publish extensions.
pub struct PublisherIdentity(SigningKey);

impl std::fmt::Debug for PublisherIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublisherIdentity")
            .field("public_key", &encode_hex(self.0.verifying_key().as_bytes()))
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

impl PublisherIdentity {
    /// Generate a cryptographically random publisher identity.
    #[must_use]
    pub fn generate() -> Self {
        Self(SigningKey::generate(&mut OsRng))
    }

    /// Read a raw 32-byte or hex-encoded private key.
    ///
    /// # Errors
    /// Returns an error for unreadable or malformed key material.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let bytes = fs::read(path)?;
        let secret = if bytes.len() == 32 {
            bytes
        } else {
            decode_hex(
                std::str::from_utf8(&bytes)
                    .map_err(|_| RegistryError::InvalidKey)?
                    .trim(),
            )?
        };
        let secret: [u8; 32] = secret.try_into().map_err(|_| RegistryError::InvalidKey)?;
        Ok(Self(SigningKey::from_bytes(&secret)))
    }

    /// Create a new private-key file without overwriting an existing identity.
    ///
    /// # Errors
    /// Returns an error if the destination exists or cannot be securely written.
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RegistryError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(encode_hex(self.0.as_bytes()).as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }

    /// Hex-encoded public key suitable for registry identity display.
    #[must_use]
    pub fn public_key(&self) -> String {
        encode_hex(self.0.verifying_key().as_bytes())
    }
}

/// Signed metadata describing one immutable registry artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRecord {
    /// Registry schema version.
    pub schema: u16,
    /// Extension manifest identifier.
    pub id: String,
    /// Extension semantic version.
    pub version: semver::Version,
    /// Registry-relative artifact path.
    pub package_path: String,
    /// BLAKE3 digest of the canonical package envelope.
    pub package_hash: String,
    /// Ed25519 publisher public key.
    pub publisher_key: String,
    /// Ed25519 signature over all preceding fields.
    pub signature: String,
}

/// Validate, canonically encode, sign, and immutably publish an extension package.
///
/// # Errors
/// Returns an error for invalid packages, conflicting versions, or filesystem failures.
pub fn publish_package(
    package_path: impl AsRef<Path>,
    registry_root: impl AsRef<Path>,
    identity: &PublisherIdentity,
) -> Result<RegistryRecord, RegistryError> {
    let bytes = fs::read(package_path)?;
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(RegistryError::TooLarge);
    }
    let package = ExtensionPackage::read_from(bytes.as_slice())?;
    let mut canonical = Vec::new();
    package.write_to(&mut canonical)?;
    let relative = format!(
        "packages/{}/{}.superspace-extension",
        package.manifest.id, package.manifest.version
    );
    let mut record = RegistryRecord {
        schema: SCHEMA,
        id: package.manifest.id.clone(),
        version: package.manifest.version.clone(),
        package_path: relative,
        package_hash: encode_hex(blake3::hash(&canonical).as_bytes()),
        publisher_key: identity.public_key(),
        signature: String::new(),
    };
    record.signature = encode_hex(&identity.0.sign(&signing_payload(&record)).to_bytes());

    let root = registry_root.as_ref();
    write_immutable(&root.join(&record.package_path), &canonical)?;
    let record_path = root
        .join("index")
        .join(&record.id)
        .join(format!("{}.json", record.version));
    let mut record_bytes = serde_json::to_vec_pretty(&record)?;
    record_bytes.push(b'\n');
    write_immutable(&record_path, &record_bytes)?;
    Ok(record)
}

/// Verify registry path binding, signature, package digest, and manifest identity.
///
/// # Errors
/// Returns an error if any signed metadata or package byte has been altered.
pub fn verify_registry_package(
    record: &RegistryRecord,
    package_bytes: &[u8],
) -> Result<ExtensionPackage, RegistryError> {
    if record.schema != SCHEMA || package_bytes.len() > MAX_PACKAGE_BYTES {
        return Err(RegistryError::InvalidRecord);
    }
    let expected_path = format!(
        "packages/{}/{}.superspace-extension",
        record.id, record.version
    );
    if record.package_path != expected_path {
        return Err(RegistryError::InvalidRecord);
    }
    let key = VerifyingKey::from_bytes(
        &decode_fixed::<32>(&record.publisher_key).map_err(|_| RegistryError::InvalidKey)?,
    )
    .map_err(|_| RegistryError::InvalidKey)?;
    let signature = Signature::from_bytes(
        &decode_fixed::<64>(&record.signature).map_err(|_| RegistryError::InvalidSignature)?,
    );
    key.verify(&signing_payload(record), &signature)
        .map_err(|_| RegistryError::InvalidSignature)?;
    if encode_hex(blake3::hash(package_bytes).as_bytes()) != record.package_hash {
        return Err(RegistryError::HashMismatch);
    }
    let package = ExtensionPackage::read_from(package_bytes)?;
    if package.manifest.id != record.id || package.manifest.version != record.version {
        return Err(RegistryError::InvalidRecord);
    }
    Ok(package)
}

/// Signed extension-registry failures.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// Key material is malformed.
    #[error("publisher key is invalid")]
    InvalidKey,
    /// Registry signature is malformed or does not verify.
    #[error("registry signature is invalid")]
    InvalidSignature,
    /// Record fields violate schema or path binding.
    #[error("registry record is invalid")]
    InvalidRecord,
    /// Package digest differs from the signed record.
    #[error("registry package hash does not match")]
    HashMismatch,
    /// An immutable version already contains different data.
    #[error("registry version already exists with different content")]
    VersionConflict,
    /// Package input exceeds the safety limit.
    #[error("registry package exceeds its size limit")]
    TooLarge,
    /// Filesystem operation failed.
    #[error("registry filesystem operation failed")]
    Io(#[from] std::io::Error),
    /// Package validation failed.
    #[error("registry package is invalid")]
    Package(#[from] PackageError),
    /// Record serialization failed.
    #[error("registry record serialization failed")]
    Json(#[from] serde_json::Error),
}

fn signing_payload(record: &RegistryRecord) -> Vec<u8> {
    let version = record.version.to_string();
    [
        record.schema.to_string().as_bytes(),
        record.id.as_bytes(),
        version.as_bytes(),
        record.package_path.as_bytes(),
        record.package_hash.as_bytes(),
        record.publisher_key.as_bytes(),
    ]
    .join(&0)
}

fn write_immutable(path: &Path, bytes: &[u8]) -> Result<(), RegistryError> {
    if path.exists() {
        return if fs::read(path)? == bytes {
            Ok(())
        } else {
            Err(RegistryError::VersionConflict)
        };
    }
    let parent = path.parent().ok_or(RegistryError::InvalidRecord)?;
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(path);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(RegistryError::Io)
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(name)
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn decode_hex(value: &str) -> Result<Vec<u8>, RegistryError> {
    if !value.len().is_multiple_of(2) {
        return Err(RegistryError::InvalidKey);
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex_digit(pair[0]).ok_or(RegistryError::InvalidKey)?;
            let low = hex_digit(pair[1]).ok_or(RegistryError::InvalidKey)?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N], RegistryError> {
    decode_hex(value)?
        .try_into()
        .map_err(|_| RegistryError::InvalidKey)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;
    use crate::{Command, ExtensionManifest, INTERFACE_ID};

    fn package_bytes() -> Vec<u8> {
        let package = ExtensionPackage::new(
            ExtensionManifest {
                id: "dev.superspace.registry-demo".into(),
                name: "Registry Demo".into(),
                version: Version::new(1, 2, 3),
                interface: INTERFACE_ID.into(),
                commands: vec![Command {
                    id: "open".into(),
                    title: "Open".into(),
                    keywords: Vec::new(),
                }],
                capabilities: Vec::new(),
            },
            vec![0, b'a', b's', b'm', 0x0d, 0, 1, 0],
        )
        .expect("package");
        let mut bytes = Vec::new();
        package.write_to(&mut bytes).expect("encode");
        bytes
    }

    #[test]
    fn signed_publication_round_trips_and_detects_tampering() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("demo.superspace-extension");
        let bytes = package_bytes();
        fs::write(&source, &bytes).expect("source");
        let identity = PublisherIdentity::generate();
        let registry = directory.path().join("registry");
        let record = publish_package(&source, &registry, &identity).expect("publish");
        let stored = fs::read(registry.join(&record.package_path)).expect("stored package");
        verify_registry_package(&record, &stored).expect("verify");

        let mut tampered = stored;
        *tampered.last_mut().expect("last byte") ^= 1;
        assert!(matches!(
            verify_registry_package(&record, &tampered),
            Err(RegistryError::HashMismatch)
        ));
        let mut forged = record;
        forged.package_path = "packages/elsewhere/1.2.3.superspace-extension".into();
        assert!(matches!(
            verify_registry_package(&forged, &bytes),
            Err(RegistryError::InvalidRecord)
        ));
    }

    #[test]
    fn private_key_file_is_not_overwritten_or_debugged() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("publisher.key");
        let identity = PublisherIdentity::generate();
        identity.write_new(&path).expect("write key");
        let restored = PublisherIdentity::read(&path).expect("read key");
        assert_eq!(identity.public_key(), restored.public_key());
        assert!(!format!("{identity:?}").contains(&encode_hex(identity.0.as_bytes())));
        assert!(identity.write_new(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
