//! StructFS-native LLM transport layer for the ox agent framework.
//!
//! `ox-gate` provides codec functions for translating between the internal
//! Anthropic-format messages and various LLM provider wire formats, plus
//! usage tracking.
//!
//! ## Phase 1: Codecs
//!
//! The [`codec`] module contains provider-specific SSE parsers and request
//! translators extracted from `ox-web`:
//!
//! - [`codec::anthropic`] — Anthropic SSE parsing and usage extraction
//! - [`codec::openai`] — OpenAI request translation and SSE parsing

pub mod codec;

pub use codec::UsageInfo;
