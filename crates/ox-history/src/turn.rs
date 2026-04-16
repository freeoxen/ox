//! Per-turn transient state for real-time streaming.
//!
//! Accumulates during a turn. Committed to the message list when
//! the agent writes to "commit". All turn state clears on commit.

use ox_types::{TokenUsage, ToolStatus};
use serde::{Deserialize, Serialize};
use structfs_core_store::Value;

/// Transient state for the current in-progress turn.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TurnState {
    /// Accumulated streaming text (assistant response being built).
    pub streaming: String,
    /// Whether the agent is currently mid-turn.
    pub thinking: bool,
    /// Current tool call, if any.
    pub tool: Option<ToolStatus>,
    /// Token usage for this turn.
    pub tokens: TokenUsage,
    /// Cumulative token usage across all turns in this session.
    /// NOT cleared by `clear()` — persists for the lifetime of the thread.
    #[serde(default)]
    pub session_tokens: TokenUsage,
    /// Token usage for the most recently completed agent run.
    /// Written by the agent worker after each run_turn completes.
    /// NOT cleared by `clear()`.
    #[serde(default)]
    pub last_run_tokens: TokenUsage,
    /// Snapshot of session_tokens at the start of the current run.
    /// Used to compute live "this query so far" cost during streaming.
    /// NOT cleared by `clear()`.
    #[serde(default)]
    pub run_start_session: TokenUsage,
    /// Per-model token breakdown for accurate multi-model cost reporting.
    /// Each entry is (model_name, cumulative tokens for that model).
    /// Populated by `reconstruct_session_usage()` and by live accumulation.
    /// NOT cleared by `clear()`.
    #[serde(default)]
    pub per_model_usage: Vec<(String, TokenUsage)>,
}

impl TurnState {
    pub fn new() -> Self {
        TurnState::default()
    }

    /// Clear all turn state (called on commit).
    pub fn clear(&mut self) {
        self.streaming.clear();
        self.thinking = false;
        self.tool = None;
        self.tokens = TokenUsage::default();
    }

    /// Whether there is any in-progress content.
    pub fn is_active(&self) -> bool {
        self.thinking || !self.streaming.is_empty() || self.tool.is_some()
    }

    /// Read a turn sub-path.
    pub fn read(&self, sub_path: &str) -> Option<Value> {
        match sub_path {
            "streaming" => Some(Value::String(self.streaming.clone())),
            "thinking" => Some(Value::Bool(self.thinking)),
            "tool" => Some(match &self.tool {
                Some(tool_status) => {
                    structfs_serde_store::to_value(tool_status).unwrap_or(Value::Null)
                }
                None => Value::Null,
            }),
            "tokens" => Some(structfs_serde_store::to_value(&self.tokens).unwrap_or(Value::Null)),
            "session_tokens" => {
                Some(structfs_serde_store::to_value(&self.session_tokens).unwrap_or(Value::Null))
            }
            "last_run" => {
                Some(structfs_serde_store::to_value(&self.last_run_tokens).unwrap_or(Value::Null))
            }
            "run_start" => {
                Some(structfs_serde_store::to_value(&self.run_start_session).unwrap_or(Value::Null))
            }
            _ => None,
        }
    }

    /// Write to a turn sub-path. Returns true if the write was handled.
    pub fn write(&mut self, sub_path: &str, value: &Value) -> bool {
        match sub_path {
            "streaming" => {
                if let Value::String(text) = value {
                    self.streaming.push_str(text);
                    return true;
                }
                false
            }
            "thinking" => {
                if let Value::Bool(b) = value {
                    self.thinking = *b;
                    return true;
                }
                false
            }
            "tool" => match value {
                Value::Map(_) => {
                    match structfs_serde_store::from_value::<ToolStatus>(value.clone()) {
                        Ok(tool_status) => {
                            self.tool = Some(tool_status);
                            true
                        }
                        Err(_) => false,
                    }
                }
                Value::Null => {
                    self.tool = None;
                    true
                }
                _ => false,
            },
            "tokens" => {
                if let Value::Map(_) = value {
                    match structfs_serde_store::from_value::<TokenUsage>(value.clone()) {
                        Ok(usage) => {
                            // Accumulate into session totals
                            self.session_tokens.input_tokens += usage.input_tokens;
                            self.session_tokens.output_tokens += usage.output_tokens;
                            self.session_tokens.cache_creation_input_tokens +=
                                usage.cache_creation_input_tokens;
                            self.session_tokens.cache_read_input_tokens +=
                                usage.cache_read_input_tokens;
                            self.tokens = usage;
                            true
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
            "last_run" => {
                if let Value::Map(_) = value {
                    match structfs_serde_store::from_value::<TokenUsage>(value.clone()) {
                        Ok(usage) => {
                            self.last_run_tokens = usage;
                            true
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
            "run_start" => {
                if let Value::Map(_) = value {
                    match structfs_serde_store::from_value::<TokenUsage>(value.clone()) {
                        Ok(usage) => {
                            self.run_start_session = usage;
                            true
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
            "per_model_add" => {
                // Expects {"model": "...", "usage": {...}}
                if let Value::Map(map) = value {
                    let model = match map.get("model") {
                        Some(Value::String(s)) => s.clone(),
                        _ => return false,
                    };
                    let usage_val = match map.get("usage") {
                        Some(v @ Value::Map(_)) => v.clone(),
                        _ => return false,
                    };
                    let Ok(usage) = structfs_serde_store::from_value::<TokenUsage>(usage_val)
                    else {
                        return false;
                    };
                    if let Some(entry) = self.per_model_usage.iter_mut().find(|(m, _)| m == &model)
                    {
                        entry.1.input_tokens =
                            entry.1.input_tokens.saturating_add(usage.input_tokens);
                        entry.1.output_tokens =
                            entry.1.output_tokens.saturating_add(usage.output_tokens);
                        entry.1.cache_creation_input_tokens = entry
                            .1
                            .cache_creation_input_tokens
                            .saturating_add(usage.cache_creation_input_tokens);
                        entry.1.cache_read_input_tokens = entry
                            .1
                            .cache_read_input_tokens
                            .saturating_add(usage.cache_read_input_tokens);
                    } else {
                        self.per_model_usage.push((model, usage));
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn streaming_accumulates() {
        let mut turn = TurnState::new();
        turn.write("streaming", &Value::String("Hello".to_string()));
        turn.write("streaming", &Value::String(" world".to_string()));
        assert_eq!(
            turn.read("streaming"),
            Some(Value::String("Hello world".to_string()))
        );
    }

    #[test]
    fn thinking_toggles() {
        let mut turn = TurnState::new();
        assert_eq!(turn.read("thinking"), Some(Value::Bool(false)));
        turn.write("thinking", &Value::Bool(true));
        assert_eq!(turn.read("thinking"), Some(Value::Bool(true)));
    }

    #[test]
    fn tool_state() {
        let mut turn = TurnState::new();
        assert_eq!(turn.read("tool"), Some(Value::Null));

        let mut tool_map = BTreeMap::new();
        tool_map.insert("name".to_string(), Value::String("bash".to_string()));
        tool_map.insert("status".to_string(), Value::String("running".to_string()));
        turn.write("tool", &Value::Map(tool_map));

        let val = turn.read("tool").unwrap();
        if let Value::Map(m) = val {
            assert_eq!(m.get("name").unwrap(), &Value::String("bash".to_string()));
        } else {
            panic!("expected map");
        }
    }

    #[test]
    fn tokens_tracking() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("input_tokens".to_string(), Value::Integer(100));
        map.insert("output_tokens".to_string(), Value::Integer(50));
        turn.write("tokens", &Value::Map(map));
        assert_eq!(
            turn.tokens,
            TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            }
        );
        // Session tokens should accumulate
        assert_eq!(
            turn.session_tokens,
            TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            }
        );
    }

    #[test]
    fn session_tokens_accumulate_across_clears() {
        let mut turn = TurnState::new();

        // First turn
        let mut map = BTreeMap::new();
        map.insert("input_tokens".to_string(), Value::Integer(100));
        map.insert("output_tokens".to_string(), Value::Integer(50));
        turn.write("tokens", &Value::Map(map));
        turn.clear();

        // Turn tokens reset, session tokens persist
        assert_eq!(turn.tokens, TokenUsage::default());
        assert_eq!(
            turn.session_tokens,
            TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            }
        );

        // Second turn
        let mut map2 = BTreeMap::new();
        map2.insert("input_tokens".to_string(), Value::Integer(200));
        map2.insert("output_tokens".to_string(), Value::Integer(80));
        turn.write("tokens", &Value::Map(map2));

        // Session tokens accumulated both turns
        assert_eq!(
            turn.session_tokens,
            TokenUsage {
                input_tokens: 300,
                output_tokens: 130,
                ..Default::default()
            }
        );
    }

    #[test]
    fn session_tokens_readable() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("input_tokens".to_string(), Value::Integer(500));
        map.insert("output_tokens".to_string(), Value::Integer(200));
        turn.write("tokens", &Value::Map(map));

        let val = turn.read("session_tokens").unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("input_tokens"), Some(&Value::Integer(500)));
                assert_eq!(m.get("output_tokens"), Some(&Value::Integer(200)));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn clear_resets_everything() {
        let mut turn = TurnState::new();
        turn.write("streaming", &Value::String("text".to_string()));
        turn.write("thinking", &Value::Bool(true));
        turn.clear();
        assert_eq!(turn.streaming, "");
        assert!(!turn.thinking);
        assert!(!turn.is_active());
    }

    #[test]
    fn is_active_reflects_state() {
        let mut turn = TurnState::new();
        assert!(!turn.is_active());
        turn.write("thinking", &Value::Bool(true));
        assert!(turn.is_active());
    }

    #[test]
    fn is_active_with_streaming_only() {
        let mut turn = TurnState::new();
        turn.write("streaming", &Value::String("text".to_string()));
        assert!(turn.is_active());
    }

    #[test]
    fn is_active_with_tool_only() {
        let mut turn = TurnState::new();
        let mut tool_map = BTreeMap::new();
        tool_map.insert("name".to_string(), Value::String("bash".to_string()));
        tool_map.insert("status".to_string(), Value::String("running".to_string()));
        turn.write("tool", &Value::Map(tool_map));
        assert!(turn.is_active());
    }

    #[test]
    fn clear_resets_tool_and_tokens() {
        let mut turn = TurnState::new();
        let mut tool_map = BTreeMap::new();
        tool_map.insert("name".to_string(), Value::String("bash".to_string()));
        tool_map.insert("status".to_string(), Value::String("done".to_string()));
        turn.write("tool", &Value::Map(tool_map));
        let mut tokens = BTreeMap::new();
        tokens.insert("input_tokens".to_string(), Value::Integer(100));
        tokens.insert("output_tokens".to_string(), Value::Integer(50));
        turn.write("tokens", &Value::Map(tokens));

        turn.clear();
        assert!(turn.tool.is_none());
        assert_eq!(turn.tokens, TokenUsage::default());
    }

    #[test]
    fn tool_clear_via_null() {
        let mut turn = TurnState::new();
        let mut tool_map = BTreeMap::new();
        tool_map.insert("name".to_string(), Value::String("bash".to_string()));
        tool_map.insert("status".to_string(), Value::String("running".to_string()));
        turn.write("tool", &Value::Map(tool_map));
        assert!(turn.tool.is_some());

        // Clear tool by writing Null
        assert!(turn.write("tool", &Value::Null));
        assert!(turn.tool.is_none());
    }

    #[test]
    fn streaming_write_wrong_type_returns_false() {
        let mut turn = TurnState::new();
        assert!(!turn.write("streaming", &Value::Integer(42)));
        assert!(turn.streaming.is_empty());
    }

    #[test]
    fn thinking_write_wrong_type_returns_false() {
        let mut turn = TurnState::new();
        assert!(!turn.write("thinking", &Value::String("yes".to_string())));
        assert!(!turn.thinking);
    }

    #[test]
    fn tool_write_non_map_non_null_returns_false() {
        let mut turn = TurnState::new();
        assert!(!turn.write("tool", &Value::String("bash".to_string())));
        assert!(turn.tool.is_none());
    }

    #[test]
    fn tool_write_map_missing_name_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("status".to_string(), Value::String("running".to_string()));
        assert!(!turn.write("tool", &Value::Map(map)));
        assert!(turn.tool.is_none());
    }

    #[test]
    fn tool_write_map_missing_status_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("name".to_string(), Value::String("bash".to_string()));
        assert!(!turn.write("tool", &Value::Map(map)));
        assert!(turn.tool.is_none());
    }

    #[test]
    fn tool_write_map_non_string_values_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("name".to_string(), Value::Integer(1));
        map.insert("status".to_string(), Value::Integer(2));
        assert!(!turn.write("tool", &Value::Map(map)));
    }

    #[test]
    fn tokens_write_wrong_type_returns_false() {
        let mut turn = TurnState::new();
        assert!(!turn.write("tokens", &Value::String("100".to_string())));
        assert_eq!(turn.tokens, TokenUsage::default());
    }

    #[test]
    fn tokens_write_map_missing_input_tokens_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("output_tokens".to_string(), Value::Integer(50));
        assert!(!turn.write("tokens", &Value::Map(map)));
    }

    #[test]
    fn tokens_write_map_missing_output_tokens_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("input_tokens".to_string(), Value::Integer(100));
        assert!(!turn.write("tokens", &Value::Map(map)));
    }

    #[test]
    fn tokens_write_map_non_integer_values_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("input_tokens".to_string(), Value::String("100".to_string()));
        map.insert("output_tokens".to_string(), Value::String("50".to_string()));
        assert!(!turn.write("tokens", &Value::Map(map)));
    }

    #[test]
    fn read_unknown_subpath_returns_none() {
        let turn = TurnState::new();
        assert!(turn.read("nonexistent").is_none());
    }

    #[test]
    fn write_unknown_subpath_returns_false() {
        let mut turn = TurnState::new();
        assert!(!turn.write("nonexistent", &Value::Null));
    }

    #[test]
    fn tokens_read_returns_map() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("input_tokens".to_string(), Value::Integer(200));
        map.insert("output_tokens".to_string(), Value::Integer(75));
        turn.write("tokens", &Value::Map(map));

        let val = turn.read("tokens").unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("input_tokens"), Some(&Value::Integer(200)));
                assert_eq!(m.get("output_tokens"), Some(&Value::Integer(75)));
            }
            _ => panic!("expected map"),
        }
    }
}
