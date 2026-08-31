use std::collections::HashSet;
use std::path::{Component, Path};

use thiserror::Error;

use crate::Capability;

/// User-approved authority for one installed extension.
#[derive(Clone, Debug, Default)]
pub struct CapabilityPolicy {
    approved: HashSet<Capability>,
}

impl CapabilityPolicy {
    /// Construct a policy from grants approved by the user.
    #[must_use]
    pub fn new(grants: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            approved: grants.into_iter().collect(),
        }
    }

    /// Require an exact grant. Broader or wildcard matching is never implicit.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::Denied`] unless the grant was explicitly approved.
    pub fn require(&self, capability: &Capability) -> Result<(), PolicyError> {
        if self.approved.contains(capability) {
            Ok(())
        } else {
            Err(PolicyError::Denied)
        }
    }

    /// Require an exact network origin present in an approved network grant.
    ///
    /// # Errors
    /// Returns [`PolicyError::Denied`] for an unapproved origin.
    pub fn require_network(&self, origin: &str) -> Result<(), PolicyError> {
        self.approved
            .iter()
            .any(|capability| {
                matches!(capability, Capability::Network(grant) if grant.origins.iter().any(|approved| approved == origin))
            })
            .then_some(())
            .ok_or(PolicyError::Denied)
    }

    /// Require an executable explicitly named in an approved process grant.
    ///
    /// # Errors
    /// Returns [`PolicyError::Denied`] for an unapproved executable.
    pub fn require_process(&self, executable: &str) -> Result<(), PolicyError> {
        self.approved
            .iter()
            .any(|capability| {
                matches!(capability, Capability::Process(executables) if executables.iter().any(|approved| approved == executable))
            })
            .then_some(())
            .ok_or(PolicyError::Denied)
    }

    /// Require a canonical absolute path contained by an approved canonical filesystem root.
    ///
    /// Callers must canonicalize existing targets, or their nearest existing parent for creates,
    /// before checking. This prevents lexical traversal and symlink escapes.
    ///
    /// # Errors
    /// Returns [`PolicyError::Denied`] for relative, traversing, read-only, or unapproved paths.
    pub fn require_filesystem(&self, path: &Path, write: bool) -> Result<(), PolicyError> {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(PolicyError::Denied);
        }
        self.approved
            .iter()
            .any(|capability| {
                let Capability::Filesystem(grant) = capability else {
                    return false;
                };
                let root = Path::new(&grant.path);
                root.is_absolute() && path.starts_with(root) && (!write || grant.write)
            })
            .then_some(())
            .ok_or(PolicyError::Denied)
    }
}

/// Capability-policy denial.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PolicyError {
    /// The extension did not receive this exact authority.
    #[error("extension capability was not approved")]
    Denied,
}
