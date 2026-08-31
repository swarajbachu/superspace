use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::{EXTENSION_WIT, ExtensionManifest, ExtensionPackage, PackageError};

/// Create an original Rust component project without overwriting an existing path.
///
/// # Errors
///
/// Returns [`DeveloperError`] for invalid identifiers, an occupied destination, or filesystem
/// failures.
pub fn scaffold_extension(
    directory: impl AsRef<Path>,
    id: &str,
    name: &str,
) -> Result<(), DeveloperError> {
    validate_scaffold_identity(id, name)?;
    let directory = directory.as_ref();
    fs::create_dir(directory).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            DeveloperError::DestinationExists
        } else {
            DeveloperError::Io(error)
        }
    })?;
    let result = (|| {
        fs::create_dir(directory.join("src"))?;
        fs::create_dir(directory.join("wit"))?;
        write_new(
            &directory.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"MIT\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[dependencies]\nwit-bindgen = \"0.46\"\n",
                rust_package_name(id)
            )
            .as_bytes(),
        )?;
        write_new(
            &directory.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "name": name,
                "version": "0.1.0",
                "interface": crate::INTERFACE_ID,
                "commands": [{"id": "open", "title": format!("Open {name}"), "keywords": []}],
                "capabilities": []
            }))?
            .as_bytes(),
        )?;
        write_new(
            &directory.join("wit/extension.wit"),
            EXTENSION_WIT.as_bytes(),
        )?;
        write_new(
            &directory.join("src/lib.rs"),
            b"wit_bindgen::generate!({ path: \"wit\", world: \"extension\" });\n\nstruct Extension;\n\nimpl Guest for Extension {\n    fn run(_command: String, _query: String) -> Result<(), String> { Ok(()) }\n    fn action(_id: String) -> Result<(), String> { Ok(()) }\n}\n\nexport!(Extension);\n",
        )?;
        write_new(
            &directory.join("README.md"),
            format!(
                "# {name}\n\nA Superspace extension implementing `{}`.\n",
                crate::INTERFACE_ID
            )
            .as_bytes(),
        )?;
        Ok::<(), DeveloperError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(directory);
    }
    result
}

/// Validate a manifest/component pair and write a deterministic package atomically.
///
/// # Errors
///
/// Returns manifest, component, package, path, or filesystem failures.
pub fn package_component(
    manifest_path: impl AsRef<Path>,
    component_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
) -> Result<[u8; 32], DeveloperError> {
    let manifest: ExtensionManifest =
        serde_json::from_slice(&read_bounded(manifest_path.as_ref(), 1024 * 1024)?)?;
    let component = read_bounded(component_path.as_ref(), 64 * 1024 * 1024)?;
    let package = ExtensionPackage::new(manifest, component)?;
    let output_path = output_path.as_ref();
    let parent = output_path.parent().ok_or(DeveloperError::InvalidPath)?;
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(output_path);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let result = (|| {
        package.write_to(&mut file)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, output_path)?;
        Ok::<(), DeveloperError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(package.module_hash)
}

/// Parse and fully revalidate an untrusted package from disk.
///
/// # Errors
///
/// Returns size, I/O, manifest, component, or integrity failures.
pub fn validate_package(path: impl AsRef<Path>) -> Result<ExtensionPackage, DeveloperError> {
    let bytes = read_bounded(path.as_ref(), 66 * 1024 * 1024)?;
    ExtensionPackage::read_from(bytes.as_slice()).map_err(DeveloperError::Package)
}

/// Install a validated package under `root/<id>/<version>.superspace-extension` atomically.
///
/// Reinstalling identical bytes is idempotent. Reusing an existing semantic version for different
/// content fails, preserving update and rollback integrity.
///
/// # Errors
///
/// Returns package validation, version conflict, path, or filesystem failures.
pub fn install_package(
    package_path: impl AsRef<Path>,
    root: impl AsRef<Path>,
) -> Result<InstallReceipt, DeveloperError> {
    let package = validate_package(package_path)?;
    let id_root = root.as_ref().join(&package.manifest.id);
    fs::create_dir_all(&id_root)?;
    let destination = id_root.join(format!("{}.superspace-extension", package.manifest.version));
    let mut canonical = Vec::new();
    package.write_to(&mut canonical)?;
    let package_hash = *blake3::hash(&canonical).as_bytes();
    if destination.exists() {
        let existing = read_bounded(&destination, 66 * 1024 * 1024)?;
        if blake3::hash(&existing).as_bytes() != &package_hash {
            return Err(DeveloperError::VersionConflict);
        }
    } else {
        let temporary = temporary_path(&destination);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let result = (|| {
            file.write_all(&canonical)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &destination)?;
            Ok::<(), DeveloperError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    }
    atomic_marker(
        &id_root.join("current"),
        package.manifest.version.to_string().as_bytes(),
    )?;
    Ok(InstallReceipt {
        id: package.manifest.id,
        version: package.manifest.version,
        path: destination,
        package_hash,
    })
}

/// Result of a validated extension installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallReceipt {
    /// Extension identifier.
    pub id: String,
    /// Installed semantic version.
    pub version: semver::Version,
    /// Canonical package path.
    pub path: PathBuf,
    /// BLAKE3 digest of the complete canonical package envelope.
    pub package_hash: [u8; 32],
}

/// Extension developer workflow failures.
#[derive(Debug, thiserror::Error)]
pub enum DeveloperError {
    /// Destination already exists and was not modified.
    #[error("destination already exists")]
    DestinationExists,
    /// Extension ID or display name is invalid.
    #[error("extension identity is invalid")]
    InvalidIdentity,
    /// Output path has no safe parent directory.
    #[error("extension output path is invalid")]
    InvalidPath,
    /// Existing installation has different bytes under the same version.
    #[error("extension version is already installed with different content")]
    VersionConflict,
    /// Input exceeds its documented limit.
    #[error("extension input exceeds its size limit")]
    TooLarge,
    /// Filesystem operation failed.
    #[error("extension developer filesystem operation failed")]
    Io(#[from] std::io::Error),
    /// Manifest JSON failed schema decoding.
    #[error("extension manifest JSON is invalid")]
    Json(#[from] serde_json::Error),
    /// Package or component validation failed.
    #[error("extension package validation failed")]
    Package(#[from] PackageError),
}

fn validate_scaffold_identity(id: &str, name: &str) -> Result<(), DeveloperError> {
    if name.trim().is_empty()
        || id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        Err(DeveloperError::InvalidIdentity)
    } else {
        Ok(())
    }
}

fn rust_package_name(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, DeveloperError> {
    let mut file = FileWithLimit::open(path, maximum)?;
    let capacity = usize::try_from(file.length).map_err(|_| DeveloperError::TooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

struct FileWithLimit {
    file: fs::File,
    length: u64,
}

impl FileWithLimit {
    fn open(path: &Path, maximum: u64) -> Result<Self, DeveloperError> {
        let file = fs::File::open(path)?;
        let length = file.metadata()?.len();
        if length > maximum {
            return Err(DeveloperError::TooLarge);
        }
        Ok(Self { file, length })
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), DeveloperError> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn temporary_path(destination: &Path) -> PathBuf {
    destination.with_extension(format!("partial-{}", std::process::id()))
}

fn atomic_marker(destination: &Path, bytes: &[u8]) -> Result<(), DeveloperError> {
    let temporary = temporary_path(destination);
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    write_new(&temporary, bytes)?;
    fs::rename(temporary, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;
    use crate::{Command, INTERFACE_ID};

    fn manifest() -> ExtensionManifest {
        ExtensionManifest {
            id: "dev.superspace.demo".into(),
            name: "Demo".into(),
            version: Version::new(1, 0, 0),
            interface: INTERFACE_ID.into(),
            commands: vec![Command {
                id: "open".into(),
                title: "Open Demo".into(),
                keywords: vec![],
            }],
            capabilities: vec![],
        }
    }

    #[test]
    fn scaffolding_is_complete_and_never_overwrites() {
        let root = tempfile::tempdir().expect("root");
        let project = root.path().join("demo");
        scaffold_extension(&project, "dev.superspace.demo", "Demo").expect("scaffold");
        for path in [
            "Cargo.toml",
            "manifest.json",
            "src/lib.rs",
            "wit/extension.wit",
            "README.md",
        ] {
            assert!(project.join(path).is_file(), "missing {path}");
        }
        assert!(matches!(
            scaffold_extension(&project, "dev.superspace.demo", "Demo"),
            Err(DeveloperError::DestinationExists)
        ));
    }

    #[test]
    fn package_validate_and_install_are_atomic_and_idempotent() {
        let root = tempfile::tempdir().expect("root");
        let manifest_path = root.path().join("manifest.json");
        let component_path = root.path().join("component.wasm");
        let package_path = root.path().join("demo.superspace-extension");
        fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest()).expect("manifest JSON"),
        )
        .expect("write manifest");
        fs::write(&component_path, [0, b'a', b's', b'm', 0x0d, 0, 1, 0]).expect("write component");
        package_component(&manifest_path, &component_path, &package_path).expect("package");
        assert_eq!(
            validate_package(&package_path).expect("validate").manifest,
            manifest()
        );
        let install_root = root.path().join("installed");
        let first = install_package(&package_path, &install_root).expect("install");
        let second = install_package(&package_path, &install_root).expect("reinstall");
        assert_eq!(first, second);
        assert_eq!(
            fs::read_to_string(install_root.join("dev.superspace.demo/current"))
                .expect("current marker"),
            "1.0.0"
        );
    }
}
