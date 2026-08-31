//! Platform-independent Superspace domain types and policies.

mod catalog;
mod preferences;
mod search;

pub use catalog::{BuiltinFeature, FeatureArea, builtin_features};
pub use preferences::{LauncherPreference, LauncherPreferences, PreferencesError};
pub use search::{SearchCandidate, SearchMatch, rank_candidates};
