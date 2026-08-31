//! Platform-independent Superspace domain types and policies.

mod catalog;
mod search;

pub use catalog::{BuiltinFeature, FeatureArea, builtin_features};
pub use search::{SearchCandidate, SearchMatch, rank_candidates};
