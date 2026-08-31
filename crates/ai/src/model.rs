use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported hosted, compatible, local, and system AI routes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// `OpenAI` Responses/Chat APIs.
    OpenAi,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini API.
    Gemini,
    /// `OpenRouter`'s `OpenAI`-compatible gateway.
    OpenRouter,
    /// User-configured `OpenAI`-compatible server.
    OpenAiCompatible,
    /// Local Ollama server.
    Ollama,
    /// Local LM Studio server.
    LmStudio,
    /// Codex route supplied by the installed Codex integration.
    Codex,
    /// Apple Intelligence route on supported macOS versions.
    AppleIntelligence,
}

/// Provider endpoint and model selection. Credentials are held separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Provider protocol and authentication behavior.
    pub provider: Provider,
    /// Model identifier sent to the provider.
    pub model: String,
    /// Optional custom endpoint, required for compatible/local routes.
    pub endpoint: Option<String>,
}

impl ProviderConfig {
    /// Resolve the configured endpoint or the provider's official default.
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref().or(match self.provider {
            Provider::OpenAi => Some("https://api.openai.com/v1"),
            Provider::Anthropic => Some("https://api.anthropic.com/v1"),
            Provider::Gemini => Some("https://generativelanguage.googleapis.com/v1beta"),
            Provider::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Provider::Ollama => Some("http://127.0.0.1:11434"),
            Provider::LmStudio => Some("http://127.0.0.1:1234/v1"),
            Provider::OpenAiCompatible | Provider::Codex | Provider::AppleIntelligence => None,
        })
    }
}

/// Opaque API credential loaded transiently from the operating-system credential store.
#[derive(Clone, Eq, PartialEq)]
pub struct Credential(String);

impl Credential {
    /// Wrap a non-empty secret for a provider request.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()).then_some(Self(value))
    }

    /// Borrow the secret only at the request boundary.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credential([REDACTED])")
    }
}

/// Conversation participant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System instruction.
    System,
    /// Human input.
    User,
    /// Model output.
    Assistant,
}

/// File or image attached by content hash rather than embedded in history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    /// Display filename.
    pub name: String,
    /// Internet media type.
    pub media_type: String,
    /// Content-addressed blob hash.
    pub blob_hash: String,
}

/// One durable conversation message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    /// Stable message identifier.
    pub id: Uuid,
    /// Participant role.
    pub role: Role,
    /// Markdown-compatible text.
    pub content: String,
    /// Referenced attachments.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Creation time in Unix milliseconds.
    pub created_at: i64,
}

/// Durable chat session with independently selectable model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    /// Stable conversation identifier.
    pub id: Uuid,
    /// User-visible title.
    pub title: String,
    /// Provider/model used for the next turn.
    pub route: ProviderConfig,
    /// Ordered message history.
    pub messages: Vec<ChatMessage>,
}

/// Reusable transform shown in the AI Quick Actions menu.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuickAction {
    /// Stable action identifier.
    pub id: String,
    /// User-visible title.
    pub title: String,
    /// Prompt template containing exactly one `{{selection}}` placeholder.
    pub prompt: String,
}

impl QuickAction {
    /// Expand this action with selected text.
    #[must_use]
    pub fn render(&self, selection: &str) -> Option<String> {
        (self.prompt.matches("{{selection}}").count() == 1)
            .then(|| self.prompt.replace("{{selection}}", selection))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_and_quick_actions_require_one_placeholder() {
        let credential = Credential::new("top-secret").expect("credential");
        assert_eq!(format!("{credential:?}"), "Credential([REDACTED])");
        assert!(!format!("{credential:?}").contains(credential.expose()));
        let action = QuickAction {
            id: "rewrite".into(),
            title: "Rewrite".into(),
            prompt: "Improve:\n{{selection}}".into(),
        };
        assert_eq!(action.render("hello").as_deref(), Some("Improve:\nhello"));
    }

    #[test]
    fn provider_defaults_keep_local_routes_on_loopback() {
        let config = ProviderConfig {
            provider: Provider::Ollama,
            model: "qwen".into(),
            endpoint: None,
        };
        assert_eq!(config.endpoint(), Some("http://127.0.0.1:11434"));
    }
}
