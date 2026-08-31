use std::collections::HashSet;

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
}

/// Capability-policy denial.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PolicyError {
    /// The extension did not receive this exact authority.
    #[error("extension capability was not approved")]
    Denied,
}
