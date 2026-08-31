use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const VERSION: u32 = 1;
const MAX_ALIAS_CHARS: usize = 80;

/// User-controlled ranking metadata for one launcher item.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LauncherPreference {
    /// Optional searchable display alias.
    pub alias: Option<String>,
    /// Explicit pin state.
    pub favorite: bool,
    /// Successful invocation count used as a bounded ranking signal.
    pub frequency: u32,
}

/// Versioned launcher preferences indexed by stable command or application ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LauncherPreferences {
    version: u32,
    items: BTreeMap<String, LauncherPreference>,
}

impl Default for LauncherPreferences {
    fn default() -> Self {
        Self {
            version: VERSION,
            items: BTreeMap::new(),
        }
    }
}

impl LauncherPreferences {
    /// Load preferences, returning defaults when the file does not exist.
    ///
    /// # Errors
    /// Returns an error for unreadable, malformed, invalid, or unsupported files.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, PreferencesError> {
        let path = path.as_ref();
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let preferences: Self = serde_json::from_slice(&bytes)?;
        preferences.validate()?;
        Ok(preferences)
    }

    /// Atomically persist preferences next to the destination file.
    ///
    /// # Errors
    /// Returns an error when validation, serialization, or filesystem access fails.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), PreferencesError> {
        self.validate()?;
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    /// Look up metadata without creating it.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&LauncherPreference> {
        self.items.get(id)
    }

    /// Set or clear a searchable alias after normalization.
    ///
    /// # Errors
    /// Returns an error when the item ID or alias violates bounded input rules.
    pub fn set_alias(&mut self, id: &str, alias: Option<&str>) -> Result<(), PreferencesError> {
        validate_id(id)?;
        let alias = alias.map(str::trim).filter(|alias| !alias.is_empty());
        if alias.is_some_and(|alias| alias.chars().count() > MAX_ALIAS_CHARS) {
            return Err(PreferencesError::InvalidAlias);
        }
        self.items.entry(id.into()).or_default().alias = alias.map(str::to_owned);
        self.prune(id);
        Ok(())
    }

    /// Toggle a launcher's pinned state and return its new value.
    ///
    /// # Errors
    /// Returns an error when the item ID is invalid.
    pub fn toggle_favorite(&mut self, id: &str) -> Result<bool, PreferencesError> {
        validate_id(id)?;
        let preference = self.items.entry(id.into()).or_default();
        preference.favorite = !preference.favorite;
        Ok(preference.favorite)
    }

    /// Record a successful invocation without allowing integer wraparound.
    ///
    /// # Errors
    /// Returns an error when the item ID is invalid.
    pub fn record_invocation(&mut self, id: &str) -> Result<u32, PreferencesError> {
        validate_id(id)?;
        let preference = self.items.entry(id.into()).or_default();
        preference.frequency = preference.frequency.saturating_add(1);
        Ok(preference.frequency)
    }

    fn validate(&self) -> Result<(), PreferencesError> {
        if self.version != VERSION {
            return Err(PreferencesError::UnsupportedVersion(self.version));
        }
        for (id, preference) in &self.items {
            validate_id(id)?;
            if preference.alias.as_ref().is_some_and(|alias| {
                alias.trim() != alias || alias.is_empty() || alias.chars().count() > MAX_ALIAS_CHARS
            }) {
                return Err(PreferencesError::InvalidAlias);
            }
        }
        Ok(())
    }

    fn prune(&mut self, id: &str) {
        if self.items.get(id).is_some_and(|preference| {
            preference.alias.is_none() && !preference.favorite && preference.frequency == 0
        }) {
            self.items.remove(id);
        }
    }
}

fn validate_id(id: &str) -> Result<(), PreferencesError> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err(PreferencesError::InvalidId);
    }
    Ok(())
}

/// Preference persistence or validation failure.
#[derive(Debug, Error)]
pub enum PreferencesError {
    /// Filesystem operation failed.
    #[error("launcher preference I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// JSON was malformed.
    #[error("launcher preferences are malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// The file requires a newer reader.
    #[error("unsupported launcher preference version {0}")]
    UnsupportedVersion(u32),
    /// Stable item ID was empty, oversized, or contained control characters.
    #[error("invalid launcher item ID")]
    InvalidId,
    /// Alias was oversized or not normalized.
    #[error("invalid launcher alias")]
    InvalidAlias,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_and_update_ranking_signals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("launcher.json");
        let mut preferences = LauncherPreferences::load(&path).unwrap();
        preferences
            .set_alias("app:terminal", Some("  Console  "))
            .unwrap();
        assert!(preferences.toggle_favorite("app:terminal").unwrap());
        assert_eq!(preferences.record_invocation("app:terminal").unwrap(), 1);
        preferences.save(&path).unwrap();

        let restored = LauncherPreferences::load(&path).unwrap();
        assert_eq!(
            restored.get("app:terminal"),
            Some(&LauncherPreference {
                alias: Some("Console".into()),
                favorite: true,
                frequency: 1,
            })
        );
    }

    #[test]
    fn validation_rejects_bad_ids_aliases_and_versions() {
        let mut preferences = LauncherPreferences::default();
        assert!(matches!(
            preferences.set_alias("", Some("x")),
            Err(PreferencesError::InvalidId)
        ));
        assert!(matches!(
            preferences.set_alias("valid", Some(&"x".repeat(MAX_ALIAS_CHARS + 1))),
            Err(PreferencesError::InvalidAlias)
        ));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("launcher.json");
        fs::write(&path, br#"{"version":99,"items":{}}"#).unwrap();
        assert!(matches!(
            LauncherPreferences::load(path),
            Err(PreferencesError::UnsupportedVersion(99))
        ));
    }
}
