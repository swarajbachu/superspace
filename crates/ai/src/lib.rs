//! Provider-neutral AI conversations, Quick Actions, and streaming event normalization.

mod model;
mod stream;

pub use model::{
    Attachment, ChatMessage, Conversation, Credential, Provider, ProviderConfig, QuickAction, Role,
};
pub use stream::{StreamDecoder, StreamError, StreamEvent, StreamProtocol};
