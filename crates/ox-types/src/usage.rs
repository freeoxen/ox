//! Usage-ledger record shape, shared by the host stores and the wasm
//! guests (which is why it lives here and not in ox-gate).

use serde::{Deserialize, Serialize};

/// One completion's usage line. Appended to the ledger (`gateway/usage`)
/// by the broker Block on terminal status; aggregated by the stats Block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRecord {
    pub id: String,
    pub account: String,
    pub model_id: String,
    /// Inbound dialect the client sent ("anthropic" | "openai" | "ox").
    pub dialect: String,
    /// Resolved provider.dialect used for the upstream call.
    pub upstream_dialect: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    /// Best-effort cost estimate from `pricing::model_pricing`. None when
    /// the model isn't in the pricing table — better to show absence than
    /// to lie about cost.
    pub estimated_cost_usd: Option<f64>,
}
