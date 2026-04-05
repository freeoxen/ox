//! Wasmtime-based agent runtime for ox.
//!
//! This crate provides the host-side infrastructure for running ox agent
//! components in a Wasm sandbox:
//!
//! - [`bridge`] — serialization between StructFS types and JSON strings
//! - [`engine`] — Wasmtime engine, component loader, and instantiation
//! - [`host_store`] — HostStore middleware with effect interception

pub mod bridge;
pub mod engine;
pub mod host_store;

pub use engine::{AgentModule, AgentRuntime, AgentState};
pub use host_store::{HostEffects, HostStore};
