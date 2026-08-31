use std::io::{Read, Write};

use thiserror::Error;
use wasmparser::{Parser, Payload, Validator, WasmFeatures};

use crate::{Capability, ExtensionManifest, INTERFACE_ID};

const MAGIC: &[u8; 8] = b"SUPEREXT";
const FORMAT_VERSION: u16 = 1;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_MODULE_BYTES: usize = 64 * 1024 * 1024;

/// A validated `.superspace-extension` package.
#[derive(Clone, Debug)]
pub struct ExtensionPackage {
    /// Parsed, schema-checked package manifest.
    pub manifest: ExtensionManifest,
    /// BLAKE3 digest of the exact component bytes.
    pub module_hash: [u8; 32],
    module: Vec<u8>,
}

impl ExtensionPackage {
    /// Validate a manifest and WebAssembly component and create a package.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] for invalid metadata or a non-component payload.
    pub fn new(manifest: ExtensionManifest, module: Vec<u8>) -> Result<Self, PackageError> {
        validate_manifest(&manifest)?;
        validate_component(&module)?;
        Ok(Self {
            module_hash: *blake3::hash(&module).as_bytes(),
            manifest,
            module,
        })
    }

    /// Borrow the validated WebAssembly component bytes.
    #[must_use]
    pub fn module(&self) -> &[u8] {
        &self.module
    }

    /// Write the deterministic package envelope.
    ///
    /// # Errors
    ///
    /// Returns I/O or serialization failures.
    pub fn write_to(&self, mut output: impl Write) -> Result<(), PackageError> {
        let manifest = serde_json::to_vec(&self.manifest)?;
        output.write_all(MAGIC)?;
        output.write_all(&FORMAT_VERSION.to_le_bytes())?;
        output.write_all(&usize_to_u32(manifest.len())?.to_le_bytes())?;
        output.write_all(&usize_to_u32(self.module.len())?.to_le_bytes())?;
        output.write_all(&self.module_hash)?;
        output.write_all(&manifest)?;
        output.write_all(&self.module)?;
        Ok(())
    }

    /// Parse and revalidate an untrusted package with strict allocation limits.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] for malformed, oversized, truncated, or tampered input.
    pub fn read_from(mut input: impl Read) -> Result<Self, PackageError> {
        let mut header = [0_u8; 50];
        input.read_exact(&mut header)?;
        if &header[..8] != MAGIC {
            return Err(PackageError::BadMagic);
        }
        if u16::from_le_bytes([header[8], header[9]]) != FORMAT_VERSION {
            return Err(PackageError::UnsupportedVersion);
        }
        let manifest_len = read_u32(&header[10..14]) as usize;
        let module_len = read_u32(&header[14..18]) as usize;
        if manifest_len > MAX_MANIFEST_BYTES || module_len > MAX_MODULE_BYTES {
            return Err(PackageError::TooLarge);
        }
        let expected_hash: [u8; 32] = header[18..50]
            .try_into()
            .map_err(|_| PackageError::Truncated)?;
        let mut manifest_bytes = vec![0; manifest_len];
        let mut module = vec![0; module_len];
        input.read_exact(&mut manifest_bytes)?;
        input.read_exact(&mut module)?;
        let mut trailing = [0_u8; 1];
        if input.read(&mut trailing)? != 0 {
            return Err(PackageError::TrailingData);
        }
        if blake3::hash(&module).as_bytes() != &expected_hash {
            return Err(PackageError::HashMismatch);
        }
        let manifest = serde_json::from_slice(&manifest_bytes)?;
        Self::new(manifest, module)
    }
}

/// Package validation, integrity, and I/O failures.
#[derive(Debug, Error)]
pub enum PackageError {
    /// Package header is not Superspace's envelope.
    #[error("invalid extension package magic")]
    BadMagic,
    /// Package format version is not supported.
    #[error("unsupported extension package version")]
    UnsupportedVersion,
    /// Declared package content exceeds safety limits.
    #[error("extension package exceeds size limits")]
    TooLarge,
    /// The package ended before its declared content.
    #[error("extension package is truncated")]
    Truncated,
    /// Bytes follow the declared package content.
    #[error("extension package has trailing data")]
    TrailingData,
    /// Component digest differs from the envelope.
    #[error("extension component hash does not match")]
    HashMismatch,
    /// Manifest metadata is invalid.
    #[error("invalid extension manifest: {0}")]
    Manifest(&'static str),
    /// Payload is not a valid WebAssembly component.
    #[error("invalid WebAssembly component")]
    InvalidComponent(#[source] wasmparser::BinaryReaderError),
    /// Package serialization failed.
    #[error("extension manifest serialization failed")]
    Json(#[from] serde_json::Error),
    /// Package stream failed.
    #[error("extension package I/O failed")]
    Io(#[from] std::io::Error),
}

fn validate_manifest(manifest: &ExtensionManifest) -> Result<(), PackageError> {
    if manifest.interface != INTERFACE_ID {
        return Err(PackageError::Manifest("unsupported interface"));
    }
    if !valid_identifier(&manifest.id) || manifest.name.trim().is_empty() {
        return Err(PackageError::Manifest("invalid identity"));
    }
    if manifest.commands.is_empty() {
        return Err(PackageError::Manifest("at least one command is required"));
    }
    let mut ids = std::collections::HashSet::new();
    for command in &manifest.commands {
        if !valid_identifier(&command.id)
            || command.title.trim().is_empty()
            || !ids.insert(&command.id)
        {
            return Err(PackageError::Manifest("invalid or duplicate command"));
        }
    }
    let mut capabilities = std::collections::HashSet::new();
    for capability in &manifest.capabilities {
        if !capabilities.insert(capability) || !valid_capability(capability) {
            return Err(PackageError::Manifest("invalid or duplicate capability"));
        }
    }
    Ok(())
}

fn valid_capability(capability: &Capability) -> bool {
    match capability {
        Capability::ClipboardRead | Capability::ClipboardWrite => true,
        Capability::Filesystem(grant) => {
            !grant.path.trim().is_empty() && grant.path.len() <= 4096 && !grant.path.contains('\0')
        }
        Capability::Network(grant) => {
            !grant.origins.is_empty()
                && grant.origins.len() <= 128
                && grant.origins.iter().all(|origin| {
                    origin.starts_with("https://")
                        && origin.len() <= 2048
                        && !origin[8..].is_empty()
                        && !origin[8..].contains(['/', '?', '#', '*'])
                })
        }
        Capability::Process(executables) => {
            !executables.is_empty()
                && executables.len() <= 128
                && executables
                    .iter()
                    .all(|executable| !executable.trim().is_empty() && !executable.contains('\0'))
        }
    }
}

fn validate_component(module: &[u8]) -> Result<(), PackageError> {
    if module.len() > MAX_MODULE_BYTES {
        return Err(PackageError::TooLarge);
    }
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator
        .validate_all(module)
        .map_err(PackageError::InvalidComponent)?;
    let is_component = Parser::new(0)
        .parse_all(module)
        .next()
        .is_some_and(|payload| {
            matches!(
                payload,
                Ok(Payload::Version {
                    encoding: wasmparser::Encoding::Component,
                    ..
                })
            )
        });
    if !is_component {
        return Err(PackageError::Manifest(
            "module must use the component model",
        ));
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn usize_to_u32(value: usize) -> Result<u32, PackageError> {
    u32::try_from(value).map_err(|_| PackageError::TooLarge)
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte header field"))
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;
    use crate::{Capability, Command};

    fn component() -> Vec<u8> {
        vec![0, b'a', b's', b'm', 0x0d, 0, 1, 0]
    }

    fn manifest() -> ExtensionManifest {
        ExtensionManifest {
            id: "dev.superspace.demo".into(),
            name: "Demo".into(),
            version: Version::new(1, 2, 3),
            interface: INTERFACE_ID.into(),
            commands: vec![Command {
                id: "hello".into(),
                title: "Say Hello".into(),
                keywords: vec!["demo".into()],
            }],
            capabilities: vec![Capability::ClipboardRead],
        }
    }

    #[test]
    fn package_round_trips_and_checks_integrity() {
        let package = ExtensionPackage::new(manifest(), component()).expect("valid package");
        let mut bytes = Vec::new();
        package.write_to(&mut bytes).expect("write package");
        let decoded = ExtensionPackage::read_from(bytes.as_slice()).expect("read package");
        assert_eq!(decoded.manifest, manifest());
        assert_eq!(decoded.module(), component());

        *bytes.last_mut().expect("module byte") ^= 1;
        assert!(matches!(
            ExtensionPackage::read_from(bytes.as_slice()),
            Err(PackageError::HashMismatch)
        ));
    }

    #[test]
    fn rejects_core_modules_and_duplicate_commands() {
        let core_module = [0, b'a', b's', b'm', 1, 0, 0, 0];
        assert!(ExtensionPackage::new(manifest(), core_module.to_vec()).is_err());
        let mut invalid = manifest();
        invalid.commands.push(invalid.commands[0].clone());
        assert!(ExtensionPackage::new(invalid, component()).is_err());
    }

    #[test]
    fn capability_policy_is_exact_and_deny_by_default() {
        let policy = crate::CapabilityPolicy::new([Capability::ClipboardRead]);
        assert!(policy.require(&Capability::ClipboardRead).is_ok());
        assert_eq!(
            policy.require(&Capability::ClipboardWrite),
            Err(crate::PolicyError::Denied)
        );
    }

    #[test]
    fn scoped_capabilities_validate_and_authorize_only_exact_resources() {
        use std::path::Path;

        use crate::{CapabilityPolicy, FilesystemGrant, NetworkGrant};

        let policy = CapabilityPolicy::new([
            Capability::Network(NetworkGrant {
                origins: vec!["https://api.example.test".into()],
            }),
            Capability::Filesystem(FilesystemGrant {
                path: "/tmp/superspace-extension".into(),
                write: false,
            }),
            Capability::Process(vec!["/usr/bin/printf".into()]),
        ]);
        assert!(policy.require_network("https://api.example.test").is_ok());
        assert!(policy.require_network("https://evil.example").is_err());
        assert!(
            policy
                .require_filesystem(Path::new("/tmp/superspace-extension/file"), false)
                .is_ok()
        );
        assert!(
            policy
                .require_filesystem(Path::new("/tmp/superspace-extension/file"), true)
                .is_err()
        );
        assert!(policy.require_process("/usr/bin/printf").is_ok());
        assert!(policy.require_process("/bin/sh").is_err());

        let mut invalid = manifest();
        invalid.capabilities = vec![Capability::Network(NetworkGrant {
            origins: vec!["https://*.example.test".into()],
        })];
        assert!(ExtensionPackage::new(invalid, component()).is_err());
    }
}
