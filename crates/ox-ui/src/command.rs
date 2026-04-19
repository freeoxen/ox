//! Command protocol for store writes.
//!
//! Every command write carries a Value::Map with optional fields:
//! - "txn": String — unique transaction ID for deduplication
//! - "from" or other precondition fields — validated by the store
//!
//! The path determines the action. The value carries context.

use std::collections::VecDeque;

use structfs_core_store::{Error as StoreError, Path, Record, Value};

/// Callback a store invokes to forward a resolved write to another store.
///
/// Shared shape used by [`InputStore`], [`CommandStore`], and
/// [`CommandLineStore`] — each receives a broker-connected dispatcher
/// at setup time so its synchronous `Writer::write` path can initiate
/// cross-store writes without owning the client.
///
/// [`InputStore`]: crate::InputStore
/// [`CommandStore`]: crate::CommandStore
/// [`CommandLineStore`]: crate::CommandLineStore
pub type Dispatcher = Box<dyn FnMut(&Path, Record) -> Result<Path, StoreError> + Send + Sync>;

/// Maximum number of txn IDs to remember for deduplication.
const TXN_HISTORY_SIZE: usize = 256;

/// Parsed command fields extracted from a write value.
pub struct Command {
    /// Transaction ID for deduplication (if present).
    pub txn: Option<String>,
    /// All fields from the command value.
    pub fields: std::collections::BTreeMap<String, Value>,
}

impl Command {
    /// Parse a command from a write value.
    ///
    /// Accepts Value::Map with optional "txn" field.
    /// Returns error if value is not a Map.
    pub fn parse(value: &Value) -> Result<Self, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => {
                return Err(StoreError::store(
                    "command",
                    "parse",
                    "command value must be a Map",
                ));
            }
        };
        let txn = map.get("txn").and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            _ => None,
        });
        Ok(Command {
            txn,
            fields: map.clone(),
        })
    }

    /// Get a string field from the command.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Get an integer field from the command.
    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.fields.get(key).and_then(|v| match v {
            Value::Integer(i) => Some(*i),
            _ => None,
        })
    }
}

/// Tracks recently seen txn IDs for deduplication.
pub struct TxnLog {
    seen: VecDeque<String>,
}

impl Default for TxnLog {
    fn default() -> Self {
        Self::new()
    }
}

impl TxnLog {
    pub fn new() -> Self {
        TxnLog {
            seen: VecDeque::new(),
        }
    }

    /// Check if a txn has been seen before. If not, record it.
    /// Returns true if the txn is a duplicate.
    pub fn is_duplicate(&mut self, txn: &str) -> bool {
        if self.seen.iter().any(|s| s == txn) {
            return true;
        }
        self.seen.push_back(txn.to_string());
        if self.seen.len() > TXN_HISTORY_SIZE {
            self.seen.pop_front();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn parse_command_with_txn() {
        let mut map = BTreeMap::new();
        map.insert("txn".to_string(), Value::String("abc123".to_string()));
        map.insert("from".to_string(), Value::String("t_001".to_string()));
        let cmd = Command::parse(&Value::Map(map)).unwrap();
        assert_eq!(cmd.txn.as_deref(), Some("abc123"));
        assert_eq!(cmd.get_str("from"), Some("t_001"));
    }

    #[test]
    fn parse_command_without_txn() {
        let map = BTreeMap::new();
        let cmd = Command::parse(&Value::Map(map)).unwrap();
        assert_eq!(cmd.txn, None);
    }

    #[test]
    fn parse_rejects_non_map() {
        let result = Command::parse(&Value::String("bad".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn txn_dedup_detects_duplicates() {
        let mut log = TxnLog::new();
        assert!(!log.is_duplicate("txn_1"));
        assert!(log.is_duplicate("txn_1"));
        assert!(!log.is_duplicate("txn_2"));
    }

    #[test]
    fn txn_dedup_evicts_oldest() {
        let mut log = TxnLog::new();
        for i in 0..TXN_HISTORY_SIZE {
            assert!(!log.is_duplicate(&format!("txn_{}", i)));
        }
        // txn_0 should still be in the log (size = 256, we added exactly 256)
        assert!(log.is_duplicate("txn_0"));
        // Add one more to evict txn_0
        assert!(!log.is_duplicate("txn_overflow"));
        assert!(!log.is_duplicate("txn_0")); // evicted, no longer duplicate
    }
}
