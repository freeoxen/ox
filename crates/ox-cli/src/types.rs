/// Per-thread rendering state — built from broker data each frame.
#[derive(Debug, Clone, Default)]
pub struct ThreadView {
    pub messages: Vec<ChatMessage>,
    pub thinking: bool,
}

/// A message visible in the conversation.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    AssistantChunk(String),
    CompletionMeta {
        model: String,
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
    },
    ToolCall {
        name: String,
    },
    ToolResult {
        name: String,
        output: String,
    },
    #[allow(dead_code)]
    Error(String),
}

use ox_types::Decision;

/// Approval options for the permission dialog.
pub const APPROVAL_OPTIONS: [(&str, Decision); 6] = [
    ("Allow once          (y)", Decision::AllowOnce),
    ("Allow for session   (s)", Decision::AllowSession),
    ("Allow always        (a)", Decision::AllowAlways),
    ("Deny once           (n)", Decision::DenyOnce),
    ("Deny for session      ", Decision::DenySession),
    ("Deny always         (d)", Decision::DenyAlways),
];

/// State for the rule customization editor.
/// Builds a clash Node + optional SandboxPolicy on submit.
pub struct CustomizeState {
    pub tool: String,
    /// Positional argument patterns (for shell: each word; for file tools: single path).
    pub args: Vec<String>,
    pub arg_cursor: usize,
    /// 0 = allow, 1 = deny.
    pub effect_idx: usize,
    /// 0 = once, 1 = session, 2 = always.
    pub scope_idx: usize,
    pub focus: usize,
    // Sandbox
    /// 0 = deny, 1 = allow, 2 = localhost
    pub network_idx: usize,
    /// Filesystem sandbox rules: (path, Cap bitflags as rwcdx booleans)
    pub fs_rules: Vec<FsRuleState>,
    pub fs_sub_focus: usize,
    pub fs_path_cursor: usize,
}

/// Editable state for one filesystem sandbox rule.
pub struct FsRuleState {
    pub path: String,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
    pub execute: bool,
}

impl CustomizeState {
    pub fn add_arg_field(&self) -> usize {
        self.args.len()
    }
    pub fn effect_field(&self) -> usize {
        self.args.len() + 1
    }
    pub fn scope_field(&self) -> usize {
        self.args.len() + 2
    }
    pub fn network_field(&self) -> usize {
        self.args.len() + 3
    }
    pub fn fs_start(&self) -> usize {
        self.args.len() + 4
    }
    pub fn add_fs_field(&self) -> usize {
        self.fs_start() + self.fs_rules.len()
    }
    pub fn total_fields(&self) -> usize {
        self.add_fs_field() + 1
    }
}
