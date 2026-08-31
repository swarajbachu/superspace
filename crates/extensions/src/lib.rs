//! Sandboxed extension manifests, packages, and capability policy.

mod developer;
mod host;
mod manifest;
mod package;
mod policy;
mod registry;

pub use manifest::{Capability, Command, ExtensionManifest, FilesystemGrant, NetworkGrant};
pub use package::{ExtensionPackage, PackageError};
pub use policy::{CapabilityPolicy, PolicyError};
pub use registry::{
    PublisherIdentity, RegistryError, RegistryRecord, publish_package, verify_registry_package,
};

/// Stable component-model interface implemented by Superspace extensions.
pub const INTERFACE_ID: &str = "superspace:extension@1";

/// Canonical WIT contract distributed to extension SDKs.
pub const EXTENSION_WIT: &str = include_str!("../wit/extension.wit");
pub use developer::{
    DeveloperError, InstallReceipt, install_package, package_component, scaffold_extension,
    validate_package,
};
pub use host::{Sandbox, SandboxError, SandboxLimits};
