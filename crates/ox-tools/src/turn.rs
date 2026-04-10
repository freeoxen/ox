use structfs_core_store::Value;

/// A single tool call (or other effect) that is waiting to be executed.
#[derive(Debug, Clone)]
pub struct PendingEffect {
    pub call_id: String,
    pub wire_name: String,
    pub input: serde_json::Value,
}

/// The outcome of executing a single effect.
#[derive(Debug, Clone)]
pub struct EffectOutcome {
    pub call_id: String,
    pub result: Result<Value, Value>,
}

/// Manages the pending/execute/results lifecycle for a single agent turn.
///
/// Multiple effects (tool calls, completions, …) may be batched together
/// within one turn.  The typical flow is:
///
/// 1. The kernel enqueues effects via [`enqueue_tool_call`].
/// 2. The shell reads [`pending`] and executes them.
/// 3. The shell hands outcomes back via [`submit_results`], which clears
///    the pending list.
/// 4. The kernel drains outcomes via [`take_results`].
/// 5. [`clear`] resets everything between turns.
pub struct TurnStore {
    pending: Vec<PendingEffect>,
    results: Vec<EffectOutcome>,
}

impl TurnStore {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            results: Vec::new(),
        }
    }

    /// Append a tool-call effect to the pending queue.
    pub fn enqueue_tool_call(&mut self, call_id: &str, wire_name: &str, input: serde_json::Value) {
        self.pending.push(PendingEffect {
            call_id: call_id.to_owned(),
            wire_name: wire_name.to_owned(),
            input,
        });
    }

    /// Returns the current pending effects, or `None` when the queue is empty.
    pub fn pending(&self) -> Option<&[PendingEffect]> {
        if self.pending.is_empty() {
            None
        } else {
            Some(&self.pending)
        }
    }

    /// Record the outcomes for the current batch and clear the pending queue.
    pub fn submit_results(&mut self, results: Vec<EffectOutcome>) {
        self.pending.clear();
        self.results.extend(results);
    }

    /// Drain and return all accumulated results.
    pub fn take_results(&mut self) -> Vec<EffectOutcome> {
        std::mem::take(&mut self.results)
    }

    /// Reset the store completely (pending and results).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.results.clear();
    }
}

impl Default for TurnStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pending_when_empty() {
        let store = TurnStore::new();
        assert!(store.pending().is_none());
    }

    #[test]
    fn enqueue_and_read_pending() {
        let mut store = TurnStore::new();
        store.enqueue_tool_call("call_1", "read_file", serde_json::json!({"path": "a.rs"}));
        store.enqueue_tool_call("call_2", "shell", serde_json::json!({"command": "ls"}));
        let pending = store.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].call_id, "call_1");
        assert_eq!(pending[1].call_id, "call_2");
    }

    #[test]
    fn submit_results_clears_pending() {
        let mut store = TurnStore::new();
        store.enqueue_tool_call("call_1", "read_file", serde_json::json!({"path": "a.rs"}));
        let results = vec![EffectOutcome {
            call_id: "call_1".into(),
            result: Ok(Value::String("file content".into())),
        }];
        store.submit_results(results);
        assert!(store.pending().is_none());
        let outcomes = store.take_results();
        assert_eq!(outcomes.len(), 1);
    }

    #[test]
    fn take_results_drains() {
        let mut store = TurnStore::new();
        store.enqueue_tool_call("c1", "tool", serde_json::json!({}));
        store.submit_results(vec![EffectOutcome {
            call_id: "c1".into(),
            result: Ok(Value::String("ok".into())),
        }]);
        let first = store.take_results();
        assert_eq!(first.len(), 1);
        let second = store.take_results();
        assert!(second.is_empty());
    }

    #[test]
    fn clear_resets_everything() {
        let mut store = TurnStore::new();
        store.enqueue_tool_call("c1", "tool", serde_json::json!({}));
        store.clear();
        assert!(store.pending().is_none());
        assert!(store.take_results().is_empty());
    }

    #[test]
    fn default_is_empty() {
        let store = TurnStore::default();
        assert!(store.pending().is_none());
    }
}
