//! Ingress protocol adapters: translate between one client-facing wire
//! format and morph-core's canonical `CanonicalRequest`/`CanonicalResponse`/
//! `ResponseEvent` types. Every adapter here implements
//! `morph_core::traits::ProtocolAdapter` and nothing else — protocol
//! adapters never talk to a `ProviderAdapter` directly, only through the
//! canonical types.

mod anthropic_messages;
mod ollama;
mod openai_chat;
mod util;

pub use anthropic_messages::AnthropicMessagesProtocol;
pub use ollama::OllamaProtocol;
pub use openai_chat::OpenAiChatProtocol;
