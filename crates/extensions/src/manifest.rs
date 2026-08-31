use semver::Version;
use serde::{Deserialize, Serialize};

/// An extension's declarative metadata and complete requested authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    /// Reverse-DNS stable identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Semantic package version.
    pub version: Version,
    /// Required host interface, currently `superspace:extension@1`.
    pub interface: String,
    /// Commands registered in the launcher.
    pub commands: Vec<Command>,
    /// Capabilities the user must explicitly approve.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

/// Launcher command exported by an extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    /// Stable command identifier within the extension.
    pub id: String,
    /// Display title.
    pub title: String,
    /// Optional search terms.
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// A narrowly-scoped host capability requested by an extension.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "grant")]
pub enum Capability {
    /// Read the current clipboard value.
    ClipboardRead,
    /// Replace the current clipboard value.
    ClipboardWrite,
    /// Access one filesystem subtree.
    Filesystem(FilesystemGrant),
    /// Connect to explicitly listed HTTPS origins.
    Network(NetworkGrant),
    /// Spawn an explicitly listed executable without shell expansion.
    Process(Vec<String>),
}

/// Filesystem subtree and allowed operation.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemGrant {
    /// User-visible path placeholder or absolute approved path.
    pub path: String,
    /// Whether files may be modified.
    #[serde(default)]
    pub write: bool,
}

/// Network allowlist. Wildcards are intentionally unsupported.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkGrant {
    /// Exact HTTPS origins, including scheme and optional port.
    pub origins: Vec<String>,
}
