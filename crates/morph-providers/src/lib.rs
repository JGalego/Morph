//! Provider adapters that speak two upstream wire formats:
//!
//! - [`openai::OpenAiProvider`]: the generic OpenAI Chat Completions wire
//!   format, which covers OpenAI itself as well as every
//!   OpenAI-wire-compatible backend (Azure OpenAI, Ollama, vLLM, LM Studio,
//!   OpenRouter, Together, Groq, Cerebras, Mistral, DeepSeek, xAI, ...).
//! - [`anthropic::AnthropicProvider`]: the Anthropic Messages API wire
//!   format.
//!
//! Both implement `morph_core::traits::ProviderAdapter`, the only trait a
//! new backend needs to implement to become usable by the rest of Morph.

pub mod anthropic;
pub mod openai;
mod util;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
