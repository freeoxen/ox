use serde::{Deserialize, Serialize};

/// Payload for the `ui/set_input` write path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetInput {
    pub text: String,
    pub cursor: usize,
}
