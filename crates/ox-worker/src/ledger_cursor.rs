use std::path::Path;

use ox_inbox::ledger::{LedgerBatch, read_ledger_batch, read_ledger_tail};

#[derive(Clone, Copy, Debug)]
pub struct LedgerCursorLimits {
    pub max_entries: usize,
    pub max_batch_bytes: usize,
    pub max_line_bytes: usize,
}

pub fn read_tail(
    inbox_root: &Path,
    thread_id: &str,
    limits: LedgerCursorLimits,
) -> Result<LedgerBatch, String> {
    read_ledger_tail(
        &inbox_root
            .join("threads")
            .join(thread_id)
            .join("ledger.jsonl"),
        limits.max_entries,
        limits.max_batch_bytes,
        limits.max_line_bytes,
    )
}

pub fn read_batch(
    inbox_root: &Path,
    thread_id: &str,
    from_seq: u64,
    limits: LedgerCursorLimits,
) -> Result<LedgerBatch, String> {
    read_ledger_batch(
        &inbox_root
            .join("threads")
            .join(thread_id)
            .join("ledger.jsonl"),
        from_seq,
        limits.max_entries,
        limits.max_batch_bytes,
        limits.max_line_bytes,
    )
}
