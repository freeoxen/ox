use std::collections::BTreeMap;
use structfs_core_store::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum InboxState {
    Inbox,
    Done,
}

impl InboxState {
    pub fn as_str(&self) -> &'static str {
        match self {
            InboxState::Inbox => "inbox",
            InboxState::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inbox" => Some(InboxState::Inbox),
            "done" => Some(InboxState::Done),
            _ => None,
        }
    }
}

pub use ox_types::ThreadState;

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMetadata {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub inbox_state: InboxState,
    pub thread_state: ThreadState,
    pub block_reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub token_count: i64,
    pub labels: Vec<String>,
    pub last_seq: i64,
    pub last_hash: Option<String>,
    /// Count of `user` + `assistant` entries in the thread's log.
    /// Distinct from `last_seq`: `last_seq` counts every log entry
    /// (including turn_start, turn_end, tool_call, completion_end, etc.)
    /// while `message_count` tracks just the conversational messages a
    /// user would recognize.
    pub message_count: i64,
}

impl ThreadMetadata {
    pub fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert("title".to_string(), Value::String(self.title.clone()));
        if let Some(ref pid) = self.parent_id {
            map.insert("parent_id".to_string(), Value::String(pid.clone()));
        }
        map.insert(
            "inbox_state".to_string(),
            Value::String(self.inbox_state.as_str().to_string()),
        );
        map.insert(
            "thread_state".to_string(),
            Value::String(self.thread_state.as_str().to_string()),
        );
        if let Some(ref reason) = self.block_reason {
            map.insert("block_reason".to_string(), Value::String(reason.clone()));
        }
        map.insert("created_at".to_string(), Value::Integer(self.created_at));
        map.insert("updated_at".to_string(), Value::Integer(self.updated_at));
        map.insert("token_count".to_string(), Value::Integer(self.token_count));
        map.insert("last_seq".to_string(), Value::Integer(self.last_seq));
        if let Some(ref h) = self.last_hash {
            map.insert("last_hash".to_string(), Value::String(h.clone()));
        }
        map.insert(
            "message_count".to_string(),
            Value::Integer(self.message_count),
        );
        map.insert(
            "labels".to_string(),
            Value::Array(
                self.labels
                    .iter()
                    .map(|l| Value::String(l.clone()))
                    .collect(),
            ),
        );
        Value::Map(map)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskInfo {
    pub id: String,
    pub thread_id: String,
    pub title: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TaskInfo {
    pub fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert(
            "thread_id".to_string(),
            Value::String(self.thread_id.clone()),
        );
        map.insert("title".to_string(), Value::String(self.title.clone()));
        map.insert("status".to_string(), Value::String(self.status.clone()));
        map.insert("created_at".to_string(), Value::Integer(self.created_at));
        map.insert("updated_at".to_string(), Value::Integer(self.updated_at));
        Value::Map(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_metadata_to_value_round_trip() {
        let meta = ThreadMetadata {
            id: "abc123".to_string(),
            title: "Test thread".to_string(),
            parent_id: None,
            inbox_state: InboxState::Inbox,
            thread_state: ThreadState::Running,
            block_reason: None,
            created_at: 1700000000,
            updated_at: 1700000100,
            token_count: 500,
            labels: vec!["backend".to_string()],
            last_seq: -1,
            last_hash: None,
            message_count: 0,
        };
        let value = meta.to_value();
        let Value::Map(ref map) = value else {
            panic!("expected Map")
        };
        assert_eq!(map.get("id"), Some(&Value::String("abc123".to_string())));
        assert_eq!(
            map.get("title"),
            Some(&Value::String("Test thread".to_string()))
        );
        assert_eq!(
            map.get("inbox_state"),
            Some(&Value::String("inbox".to_string()))
        );
        assert_eq!(
            map.get("thread_state"),
            Some(&Value::String("running".to_string()))
        );
        assert!(!map.contains_key("parent_id"));
        assert!(!map.contains_key("block_reason"));
        assert_eq!(map.get("token_count"), Some(&Value::Integer(500)));
        assert_eq!(
            map.get("labels"),
            Some(&Value::Array(vec![Value::String("backend".to_string())]))
        );
    }

    #[test]
    fn thread_metadata_with_parent_and_block_reason() {
        let meta = ThreadMetadata {
            id: "child1".to_string(),
            title: "Sub-task".to_string(),
            parent_id: Some("parent1".to_string()),
            inbox_state: InboxState::Inbox,
            thread_state: ThreadState::BlockedOnApproval,
            block_reason: Some("shell \"cargo test\"".to_string()),
            created_at: 1700000000,
            updated_at: 1700000200,
            token_count: 1200,
            labels: vec![],
            last_seq: -1,
            last_hash: None,
            message_count: 0,
        };
        let value = meta.to_value();
        let Value::Map(ref map) = value else {
            panic!("expected Map")
        };
        assert_eq!(
            map.get("parent_id"),
            Some(&Value::String("parent1".to_string()))
        );
        assert_eq!(
            map.get("block_reason"),
            Some(&Value::String("shell \"cargo test\"".to_string()))
        );
    }

    #[test]
    fn inbox_state_parse_round_trip() {
        assert_eq!(InboxState::parse("inbox"), Some(InboxState::Inbox));
        assert_eq!(InboxState::parse("done"), Some(InboxState::Done));
        assert_eq!(InboxState::parse("unknown"), None);
        assert_eq!(InboxState::Inbox.as_str(), "inbox");
        assert_eq!(InboxState::Done.as_str(), "done");
    }

    #[test]
    fn thread_state_parse_round_trip() {
        let states = [
            ("running", ThreadState::Running),
            ("waiting_for_input", ThreadState::WaitingForInput),
            ("blocked_on_approval", ThreadState::BlockedOnApproval),
            ("completed", ThreadState::Completed),
            ("errored", ThreadState::Errored),
        ];
        for (s, expected) in &states {
            assert_eq!(ThreadState::parse(s), Some(expected.clone()));
            assert_eq!(expected.as_str(), *s);
        }
        assert_eq!(ThreadState::parse("nope"), None);
    }

    #[test]
    fn task_info_to_value() {
        let task = TaskInfo {
            id: "t1".to_string(),
            thread_id: "abc123".to_string(),
            title: "Fix the bug".to_string(),
            status: "in_progress".to_string(),
            created_at: 1700000000,
            updated_at: 1700000050,
        };
        let value = task.to_value();
        let Value::Map(ref map) = value else {
            panic!("expected Map")
        };
        assert_eq!(map.get("id"), Some(&Value::String("t1".to_string())));
        assert_eq!(
            map.get("status"),
            Some(&Value::String("in_progress".to_string()))
        );
    }
}
