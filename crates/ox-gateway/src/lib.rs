//! Local LLM gateway: a thin axum shell over the StructFS substrate.
//!
//! See `docs/superpowers/specs/2026-05-24-ox-gateway-design.md` for the
//! architecture (codec symmetry, CompletionBrokerStore lifecycle, etc.).
//!
//! Subsequent tasks add modules here:
//!   - `error` — dialect-shaped HTTP error envelopes
//!   - `handle` — shared async drain helpers (stream + buffer)
//!   - `routes` — axum routers (anthropic, openai, models, ox_native)
