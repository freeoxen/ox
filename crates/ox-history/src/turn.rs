//! Per-turn transient state for real-time streaming.
//!
//! Accumulates during a turn. Committed to the message list when
//! the agent writes to "commit". All turn state clears on commit.

use std::collections::BTreeMap;
use structfs_core_store::Value;

/// Transient state for the current in-progress turn.
#[derive(Debug, Default)]
pub struct TurnState {
    /// Accumulated streaming text (assistant response being built).
    pub streaming: String,
    /// Whether the agent is currently mid-turn.
    pub thinking: bool,
    /// Current tool call, if any: (tool_name, status).
    pub tool: Option<(String, String)>,
    /// Token usage for this turn: (input_tokens, output_tokens).
    pub tokens: (u32, u32),
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
        self.tokens = (0, 0);
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
                Some((name, status)) => {
                    let mut map = BTreeMap::new();
                    map.insert("name".to_string(), Value::String(name.clone()));
                    map.insert("status".to_string(), Value::String(status.clone()));
                    Value::Map(map)
                }
                None => Value::Null,
            }),
            "tokens" => {
                let mut map = BTreeMap::new();
                map.insert("in".to_string(), Value::Integer(self.tokens.0 as i64));
                map.insert("out".to_string(), Value::Integer(self.tokens.1 as i64));
                Some(Value::Map(map))
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
                Value::Map(map) => {
                    let name = map.get("name").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    });
                    let status = map.get("status").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    });
                    if let (Some(n), Some(s)) = (name, status) {
                        self.tool = Some((n, s));
                        return true;
                    }
                    false
                }
                Value::Null => {
                    self.tool = None;
                    true
                }
                _ => false,
            },
            "tokens" => {
                if let Value::Map(map) = value {
                    let in_tokens = map.get("in").and_then(|v| match v {
                        Value::Integer(i) => Some(*i as u32),
                        _ => None,
                    });
                    let out_tokens = map.get("out").and_then(|v| match v {
                        Value::Integer(i) => Some(*i as u32),
                        _ => None,
                    });
                    if let (Some(i), Some(o)) = (in_tokens, out_tokens) {
                        self.tokens = (i, o);
                        return true;
                    }
                    false
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
        map.insert("in".to_string(), Value::Integer(100));
        map.insert("out".to_string(), Value::Integer(50));
        turn.write("tokens", &Value::Map(map));
        assert_eq!(turn.tokens, (100, 50));
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
        tokens.insert("in".to_string(), Value::Integer(100));
        tokens.insert("out".to_string(), Value::Integer(50));
        turn.write("tokens", &Value::Map(tokens));

        turn.clear();
        assert!(turn.tool.is_none());
        assert_eq!(turn.tokens, (0, 0));
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
        assert_eq!(turn.tokens, (0, 0));
    }

    #[test]
    fn tokens_write_map_missing_in_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("out".to_string(), Value::Integer(50));
        assert!(!turn.write("tokens", &Value::Map(map)));
    }

    #[test]
    fn tokens_write_map_missing_out_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("in".to_string(), Value::Integer(100));
        assert!(!turn.write("tokens", &Value::Map(map)));
    }

    #[test]
    fn tokens_write_map_non_integer_values_returns_false() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("in".to_string(), Value::String("100".to_string()));
        map.insert("out".to_string(), Value::String("50".to_string()));
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
        map.insert("in".to_string(), Value::Integer(200));
        map.insert("out".to_string(), Value::Integer(75));
        turn.write("tokens", &Value::Map(map));

        let val = turn.read("tokens").unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("in"), Some(&Value::Integer(200)));
                assert_eq!(m.get("out"), Some(&Value::Integer(75)));
            }
            _ => panic!("expected map"),
        }
    }
}
