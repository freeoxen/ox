//! Local LLM gateway: a thin axum shell over the StructFS substrate.

pub mod assembly;
pub mod error;
pub mod handle;
pub mod routes;
pub mod broker_block;
pub mod codec_block;
pub mod telemetry_store;
pub mod traffic;
pub mod wire_store;
