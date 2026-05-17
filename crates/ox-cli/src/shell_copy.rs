//! Shell-specific copy: synthetic ToolResult content and ledger-health
//! banner text owned by `ox-cli`.
//!
//! These strings are intentionally kept out of the generic horns toolkit
//! (which owns only the `Theme` rendering primitives) because they encode
//! ox-shell behaviour — the kernel/inbox produce only state signals, and
//! the shell owns the user-facing copy that surfaces those signals.

/// Synthetic `ToolResult` content that ox-cli writes into the structured log
/// when a user chooses **Skip** on the post-crash re-confirm modal.
///
/// The text is pinned by the plan (see
/// `docs/superpowers/plans/2026-04-19-durable-conversation-state.md`,
/// "Skip-path `ToolResult` shape (pinned)"). Changing this string is a plan
/// amendment, not a drive-by edit: the `[ox-cli:` marker prefix makes the
/// synthetic origin recognizable in transcripts, and the "Do not retry"
/// directive is load-bearing — it's the only signal the model has that
/// re-calling the tool is explicitly unwanted.
///
/// The shell seeds this constant onto the namespace at
/// `shell/post_crash_skip_content` during mount (see
/// `ThreadNamespace::new_default` and `from_thread_dir`). The kernel reads it
/// from that path on the Deny branch of the post-crash re-confirm resume
/// prologue (`ox-kernel::run::post_crash_skip_content`) and falls back to a
/// kernel-neutral `[ox: …]` default when the path is unset — which keeps
/// `ox-kernel` free of any shell-specific strings.
pub const POST_CRASH_SKIP_CONTENT: &str = "[ox-cli: skipped by user after crash recovery. \
    The tool was not re-executed. Do not retry this tool in this turn.]";

// ---------------------------------------------------------------------------
// Ledger-health banners
// ---------------------------------------------------------------------------
//
// Three terminal mount-time states surface a single-line banner at the top
// of the thread view. Copy is owned by the shell — the kernel/inbox produce
// only the state signal (`shell/ledger_health`), keeping `ox-kernel` and
// `ox-inbox` free of any shell-specific strings (mirrors the
// `POST_CRASH_SKIP_CONTENT` pattern above).
//
// Wire-string keys live in `crate::thread_registry::LEDGER_HEALTH_*`.

/// Banner shown when `ledger.jsonl` was absent at mount time.
pub const LEDGER_MISSING_BANNER: &str =
    "This thread's log is missing. No conversation state can be recovered.";

/// Banner shown when an interior line failed to parse, or torn-tail
/// truncation itself failed (read-only disk, permissions). Thread is
/// mounted read-only.
pub const LEDGER_REPAIR_FAILED_BANNER: &str =
    "This thread's log is damaged and cannot be repaired. Mounted read-only.";

/// Banner shown when a post-mount commit failed (e.g. `LedgerWriter`
/// could not be spawned, or — in the follow-up commit — a write_all /
/// sync_data hit an I/O error). Conversation is frozen for the rest of
/// this process; relaunching may recover.
pub const LEDGER_DEGRADED_BANNER: &str =
    "This thread's log cannot be written — conversation is frozen. Relaunch may recover.";

/// Map a `shell/ledger_health` wire string to the banner copy. Returns
/// `None` for `"ok"` (the no-banner case) and any unknown value.
pub fn ledger_health_banner(wire: &str) -> Option<&'static str> {
    match wire {
        crate::thread_registry::LEDGER_HEALTH_MISSING => Some(LEDGER_MISSING_BANNER),
        crate::thread_registry::LEDGER_HEALTH_REPAIR_FAILED => Some(LEDGER_REPAIR_FAILED_BANNER),
        crate::thread_registry::LEDGER_HEALTH_DEGRADED => Some(LEDGER_DEGRADED_BANNER),
        _ => None,
    }
}
