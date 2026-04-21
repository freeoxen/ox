/// Per-thread rendering state — built from broker data each frame.
#[derive(Debug, Clone, Default)]
pub struct ThreadView {
    pub messages: Vec<ChatMessage>,
    pub thinking: bool,
}

// -- Message-count authority ---------------------------------------------
//
// Three places in the tree hold a count of user+assistant messages:
//   1. `inbox.db.threads.message_count` — SQLite rollup, display of record
//      for the inbox listing. Kept live by `write_save_result_to_inbox`
//      invoked from the `CommitDrain` task (one per mounted thread) that
//      observes the `LedgerWriter`'s latest-wins slot, and by `reconcile`
//      at startup.
//   2. Derived by `aggregate_thread_stats` from `threads/{id}/log/entries`
//      — the source the info modal shows. Includes tool / model / usage
//      aggregates that the SQLite rollup doesn't carry.
//   3. `ox_inbox::snapshot::count_messages_in_ledger` — the ground-truth
//      read from `ledger.jsonl`, used at startup reconcile and as the
//      seed for the LedgerWriter's in-memory message counter.
//
// Authority hierarchy when they ever disagree: (3) is ground truth, (1)
// is the cached reflection for listings, (2) is a recompute from the
// live log with richer structure. They are kept in sync by the fact
// that `LedgerWriter` is the single writer of `ledger.jsonl` (it alone
// mutates the hash chain), and (1) is written through from its drain
// slot. `save_config_snapshot` only touches `context.json` and is not
// part of the message-count pipeline.

/// Inbox-index metadata about a thread. Cheap — read straight from the
/// SQLite rollup; does not require scanning the log.
#[derive(Debug, Clone, Default)]
pub struct ThreadMetadata {
    pub id: String,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    /// Aggregate token count from the inbox index (rollup).
    pub token_count: i64,
}

/// Stats aggregated from the thread log + turn state. Populated only
/// when the thread-info modal is open — requires a log scan.
#[derive(Debug, Clone, Default)]
pub struct ThreadStats {
    /// Total number of conversational messages (user + assistant).
    pub message_count: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    /// Distinct tool names used with a call count each (sorted by name).
    pub tool_uses: Vec<(String, usize)>,
    /// Distinct models that have appeared in this thread (in the order
    /// they first appeared).
    pub models: Vec<String>,
    /// Most recent model used — the right key for pricing lookup when
    /// only a single model is in play.
    pub primary_model: Option<String>,
    /// Session-scope token usage (cumulative across turns).
    pub session_tokens: ox_types::TokenUsage,
    /// Per-model token usage for accurate multi-model cost estimation.
    pub per_model_usage: Vec<(String, ox_types::TokenUsage)>,
}

/// Full snapshot the info modal renders: cheap metadata plus the
/// computed stats. Kept as a product of the two types so each layer's
/// responsibility stays obvious.
#[derive(Debug, Clone, Default)]
pub struct ThreadInfo {
    pub meta: ThreadMetadata,
    pub stats: ThreadStats,
}

impl ThreadInfo {
    /// Thread id — convenience pass-through so callers can key caches
    /// without reaching through `meta`.
    pub fn id(&self) -> &str {
        &self.meta.id
    }
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
