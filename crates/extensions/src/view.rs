use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_ITEMS: usize = 10_000;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_NAVIGATION_DEPTH: usize = 64;

/// User-invokable action emitted by a declarative extension view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// Stable identifier delivered to the extension's `action` export.
    pub id: String,
    /// Visible action title.
    pub title: String,
    /// Optional keyboard shortcut label.
    #[serde(default)]
    pub shortcut: Option<String>,
}

/// One row in a declarative list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListItem {
    /// Stable row identifier.
    pub id: String,
    /// Primary label.
    pub title: String,
    /// Optional secondary label.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Optional icon name or packaged asset reference.
    #[serde(default)]
    pub icon: Option<String>,
    /// Row-specific actions.
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// One card in a declarative grid.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridItem {
    /// Stable card identifier.
    pub id: String,
    /// Card title.
    pub title: String,
    /// Optional image or packaged asset reference.
    #[serde(default)]
    pub image: Option<String>,
    /// Card-specific actions.
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// Label/value row in a detail view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    /// Row label.
    pub label: String,
    /// Row value.
    pub value: String,
}

/// Supported declarative form controls.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormFieldKind {
    /// Single-line text input.
    Text,
    /// Multi-line text input.
    TextArea,
    /// Boolean checkbox.
    Checkbox,
    /// Select control with literal choices.
    Select(Vec<String>),
    /// Password input whose value must never be persisted in view history.
    Password,
}

/// One declarative form field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormField {
    /// Stable field identifier.
    pub id: String,
    /// Visible field label.
    pub label: String,
    /// Control type.
    pub kind: FormFieldKind,
    /// Whether submission requires a non-empty value.
    #[serde(default)]
    pub required: bool,
}

/// Complete declarative view tree accepted from an untrusted extension.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum View {
    /// Searchable vertical result list.
    List {
        /// Ordered list rows.
        items: Vec<ListItem>,
    },
    /// Searchable card grid.
    Grid {
        /// Ordered grid cards.
        items: Vec<GridItem>,
    },
    /// Markdown body with metadata rows and actions.
    Detail {
        /// Markdown description.
        markdown: String,
        /// Label/value metadata rows.
        metadata: Vec<Detail>,
        /// Actions available for the detail.
        actions: Vec<Action>,
    },
    /// Standalone Markdown content.
    Markdown {
        /// Markdown document.
        markdown: String,
    },
    /// Input form and submit action.
    Form {
        /// Input controls.
        fields: Vec<FormField>,
        /// Form submission action.
        submit: Action,
    },
    /// Action menu.
    Menu {
        /// Ordered menu actions.
        actions: Vec<Action>,
    },
    /// Determinate or indeterminate progress state.
    Progress {
        /// Progress operation label.
        title: String,
        /// Completion fraction, or `None` for indeterminate progress.
        fraction: Option<f32>,
        /// Whether the host should expose cancellation.
        cancellable: bool,
    },
}

impl View {
    /// Validate size limits, identifiers, uniqueness, and progress bounds.
    ///
    /// # Errors
    /// Returns [`ViewError`] when untrusted view data violates the host schema.
    pub fn validate(&self) -> Result<(), ViewError> {
        let encoded = serde_json::to_vec(self).map_err(|_| ViewError::Invalid)?;
        if encoded.len() > MAX_TEXT_BYTES {
            return Err(ViewError::TooLarge);
        }
        match self {
            Self::List { items } => {
                ensure_count(items.len())?;
                unique(items.iter().map(|item| item.id.as_str()))?;
                for item in items {
                    validate_identity(&item.id, &item.title)?;
                    validate_actions(&item.actions)?;
                }
            }
            Self::Grid { items } => {
                ensure_count(items.len())?;
                unique(items.iter().map(|item| item.id.as_str()))?;
                for item in items {
                    validate_identity(&item.id, &item.title)?;
                    validate_actions(&item.actions)?;
                }
            }
            Self::Detail {
                markdown,
                metadata,
                actions,
            } => {
                ensure_count(metadata.len())?;
                if markdown.len() > MAX_TEXT_BYTES
                    || metadata
                        .iter()
                        .any(|row| row.label.trim().is_empty() || row.value.len() > MAX_TEXT_BYTES)
                {
                    return Err(ViewError::Invalid);
                }
                validate_actions(actions)?;
            }
            Self::Markdown { markdown } => {
                if markdown.len() > MAX_TEXT_BYTES {
                    return Err(ViewError::TooLarge);
                }
            }
            Self::Form { fields, submit } => {
                ensure_count(fields.len())?;
                unique(fields.iter().map(|field| field.id.as_str()))?;
                for field in fields {
                    validate_identity(&field.id, &field.label)?;
                    if matches!(&field.kind, FormFieldKind::Select(options) if options.is_empty() || options.len() > MAX_ITEMS)
                    {
                        return Err(ViewError::Invalid);
                    }
                }
                validate_actions(std::slice::from_ref(submit))?;
            }
            Self::Menu { actions } => validate_actions(actions)?,
            Self::Progress {
                title, fraction, ..
            } => {
                if title.trim().is_empty()
                    || fraction
                        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
                {
                    return Err(ViewError::Invalid);
                }
            }
        }
        Ok(())
    }
}

/// Bounded navigation stack owned by the host rather than an extension.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Navigation {
    stack: Vec<View>,
}

impl Navigation {
    /// Push a validated view onto the navigation stack.
    ///
    /// # Errors
    /// Returns an error for invalid views or excessive nesting.
    pub fn push(&mut self, view: View) -> Result<(), ViewError> {
        view.validate()?;
        if self.stack.len() >= MAX_NAVIGATION_DEPTH {
            return Err(ViewError::TooDeep);
        }
        self.stack.push(view);
        Ok(())
    }

    /// Pop the current view, retaining the root view when present.
    pub fn pop(&mut self) -> Option<View> {
        (self.stack.len() > 1).then(|| self.stack.pop()).flatten()
    }

    /// Borrow the visible view.
    #[must_use]
    pub fn current(&self) -> Option<&View> {
        self.stack.last()
    }

    /// Current navigation depth.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

/// Declarative extension-view validation failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ViewError {
    /// A field, identifier, or value is malformed.
    #[error("extension view is invalid")]
    Invalid,
    /// The view exceeds the host's allocation limit.
    #[error("extension view is too large")]
    TooLarge,
    /// An identifier is duplicated in its scope.
    #[error("extension view contains duplicate identifiers")]
    Duplicate,
    /// Navigation exceeds the host depth limit.
    #[error("extension navigation is too deep")]
    TooDeep,
}

fn ensure_count(count: usize) -> Result<(), ViewError> {
    if count > MAX_ITEMS {
        Err(ViewError::TooLarge)
    } else {
        Ok(())
    }
}

fn validate_identity(id: &str, title: &str) -> Result<(), ViewError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || title.trim().is_empty()
    {
        Err(ViewError::Invalid)
    } else {
        Ok(())
    }
}

fn validate_actions(actions: &[Action]) -> Result<(), ViewError> {
    ensure_count(actions.len())?;
    unique(actions.iter().map(|action| action.id.as_str()))?;
    for action in actions {
        validate_identity(&action.id, &action.title)?;
    }
    Ok(())
}

fn unique<'a>(values: impl Iterator<Item = &'a str>) -> Result<(), ViewError> {
    let mut seen = HashSet::new();
    if values.into_iter().all(|value| seen.insert(value)) {
        Ok(())
    } else {
        Err(ViewError::Duplicate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_every_component_and_navigation() {
        let action = Action {
            id: "open".into(),
            title: "Open".into(),
            shortcut: Some("enter".into()),
        };
        let views = vec![
            View::List {
                items: vec![ListItem {
                    id: "one".into(),
                    title: "One".into(),
                    subtitle: None,
                    icon: None,
                    actions: vec![action.clone()],
                }],
            },
            View::Grid {
                items: vec![GridItem {
                    id: "card".into(),
                    title: "Card".into(),
                    image: None,
                    actions: Vec::new(),
                }],
            },
            View::Detail {
                markdown: "# Detail".into(),
                metadata: vec![Detail {
                    label: "Status".into(),
                    value: "Ready".into(),
                }],
                actions: vec![action.clone()],
            },
            View::Markdown {
                markdown: "**Hello**".into(),
            },
            View::Form {
                fields: vec![FormField {
                    id: "name".into(),
                    label: "Name".into(),
                    kind: FormFieldKind::Text,
                    required: true,
                }],
                submit: action.clone(),
            },
            View::Menu {
                actions: vec![action],
            },
            View::Progress {
                title: "Loading".into(),
                fraction: Some(0.5),
                cancellable: true,
            },
        ];
        let mut navigation = Navigation::default();
        for view in views {
            navigation.push(view).expect("valid view");
        }
        assert_eq!(navigation.depth(), 7);
        assert!(navigation.current().is_some());
        assert!(navigation.pop().is_some());
    }

    #[test]
    fn rejects_duplicate_ids_invalid_progress_and_excessive_depth() {
        let action = Action {
            id: "same".into(),
            title: "Same".into(),
            shortcut: None,
        };
        assert_eq!(
            View::Menu {
                actions: vec![action.clone(), action]
            }
            .validate(),
            Err(ViewError::Duplicate)
        );
        assert_eq!(
            View::Progress {
                title: "Bad".into(),
                fraction: Some(1.1),
                cancellable: false,
            }
            .validate(),
            Err(ViewError::Invalid)
        );
        let mut navigation = Navigation::default();
        for _ in 0..MAX_NAVIGATION_DEPTH {
            navigation
                .push(View::Markdown {
                    markdown: String::new(),
                })
                .expect("within limit");
        }
        assert_eq!(
            navigation.push(View::Markdown {
                markdown: String::new()
            }),
            Err(ViewError::TooDeep)
        );
    }
}
