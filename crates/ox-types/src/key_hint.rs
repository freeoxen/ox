use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyHint {
    pub key: String,
    pub description: String,
}
