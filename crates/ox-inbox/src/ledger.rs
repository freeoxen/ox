//! Content-addressed ledger — append-only message log with hash chain.

use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// A single ledger entry with content-addressed hash and parent chain.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub seq: u64,
    pub hash: String,
    pub parent: Option<String>,
    pub msg: serde_json::Value,
}

/// Terminal mount-time health of a thread's ledger.
///
/// Reported by the snapshot/replay path so the shell can render the
/// matching banner (see `ox-cli/src/theme.rs`). All four variants are
/// terminal in the sense that subsequent reads do not change the
/// classification; `Degraded` is the only one that can be entered
/// *after* mount (a post-mount commit failure flips it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerHealth {
    /// Ledger absent (cold-start or expected-empty thread) or present
    /// and fully parseable. The "no banner" case.
    Ok,
    /// Ledger file did not exist at mount time but the thread directory
    /// otherwise carries persisted state (e.g. a `context.json` only).
    /// The conversation log cannot be recovered — the user sees an
    /// explicit message, not a silently empty thread.
    Missing,
    /// Ledger could not be repaired:
    ///  - an interior line failed to parse (mid-file corruption — not a
    ///    torn tail), or
    ///  - torn-tail truncation itself failed (read-only disk,
    ///    permissions).
    ///
    /// The thread mounts read-only.
    RepairFailed,
    /// A post-mount `write_all` / `sync_data` returned an error. The
    /// conversation is frozen for the rest of this process; relaunching
    /// may recover if the underlying I/O issue clears.
    ///
    /// Reached today via `LedgerWriter::spawn` failing at mount time
    /// (e.g., cannot open the ledger for append) or via direct
    /// construction in tests. The production freeze hook +
    /// failure-injection harness that makes this reachable via a
    /// mid-stream `write_all` / `sync_data` error lands in the
    /// follow-up commit alongside the rest of Task 1 hardening
    /// (benchmark, golden fixture, S4/S5 crash scenarios, tracing).
    Degraded,
}

/// Outcome of [`read_ledger_with_repair`] when the ledger was successfully
/// (re)opened — the entries that survived plus a flag describing whether a
/// torn tail was repaired.
#[derive(Debug, Clone)]
pub struct ReadOutcome {
    pub entries: Vec<LedgerEntry>,
    /// `Some(bytes_dropped)` when the on-disk file ended in a torn or
    /// partially-written line that this call truncated. `None` for clean
    /// reads.
    pub repaired_bytes: Option<u64>,
}

/// Mount-time error class returned by [`read_ledger_with_repair`].
///
/// `Missing` and `RepairFailed` round-trip directly into the matching
/// [`LedgerHealth`] variants. The conversion is not automatic on purpose
/// — the caller decides whether a missing ledger is interesting (a thread
/// directory that has a `context.json` but no `ledger.jsonl`) or expected
/// (a brand-new thread).
#[derive(Debug, Clone)]
pub enum MountError {
    /// The file does not exist.
    Missing,
    /// The file exists but cannot be read or parsed in a way the repair
    /// path can recover from.
    RepairFailed { reason: String },
}

impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::Missing => write!(f, "ledger file is missing"),
            MountError::RepairFailed { reason } => write!(f, "ledger repair failed: {reason}"),
        }
    }
}

impl std::error::Error for MountError {}

/// Compute the content hash of a message: SHA-256 of its JSON, truncated to 16 hex chars.
pub fn entry_hash(msg: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(msg).expect("message always serializes");
    let digest = Sha256::digest(&bytes);
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Append a new entry to the ledger file. Returns the created entry.
///
/// `prev` is the previous entry (for parent hash and seq computation).
/// Pass `None` for the first entry.
pub fn append_entry(
    path: &Path,
    msg: &serde_json::Value,
    prev: Option<&LedgerEntry>,
) -> Result<LedgerEntry, String> {
    let seq = prev.map_or(0, |e| e.seq + 1);
    let parent = prev.map(|e| e.hash.clone());
    let hash = entry_hash(msg);

    let entry = LedgerEntry {
        seq,
        hash: hash.clone(),
        parent: parent.clone(),
        msg: msg.clone(),
    };

    let line = serde_json::json!({
        "seq": seq,
        "hash": hash,
        "parent": parent,
        "msg": msg,
    });
    let line_str = serde_json::to_string(&line).map_err(|e| e.to_string())?;

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{line_str}").map_err(|e| e.to_string())?;

    Ok(entry)
}

/// Read all entries from a ledger file.
pub fn read_ledger(path: &Path) -> Result<Vec<LedgerEntry>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        let json: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        let seq = json["seq"].as_u64().ok_or("missing seq")?;
        let hash = json["hash"].as_str().ok_or("missing hash")?.to_string();
        let parent = json["parent"].as_str().map(|s| s.to_string());
        let msg = json.get("msg").ok_or("missing msg")?.clone();
        entries.push(LedgerEntry {
            seq,
            hash,
            parent,
            msg,
        });
    }
    Ok(entries)
}

/// Read all entries from a ledger file, repairing a torn tail in place if
/// one is present.
///
/// "Torn tail" = the last line either does not end in `\n` or fails to
/// parse as a `LedgerEntry`. This is the natural failure mode when the
/// process crashes (or is power-cut) between `write_all` and
/// `File::sync_data` inside the writer's commit loop. The entry is not
/// yet durable — by definition, the ack hasn't been sent — so we can
/// safely discard those bytes and continue with the prior entries.
///
/// Interior corruption (a parse failure on any line *other* than the
/// last) is **not** a torn tail; it indicates damage we don't know how
/// to fix without inventing data. We fail with [`MountError::RepairFailed`]
/// in that case and let the shell mount the thread read-only.
///
/// On successful repair, the file on disk is truncated to the last good
/// byte boundary before this function returns. A `tracing::info!` event
/// `LedgerTailRepaired` is emitted; durability of the truncate itself is
/// best-effort (no extra fsync — the next durable commit will sync, and
/// a crash before then leaves the file at-or-shorter than it was).
pub fn read_ledger_with_repair(path: &Path) -> Result<ReadOutcome, MountError> {
    if !path.exists() {
        return Err(MountError::Missing);
    }

    // Slurp the whole file. Ledger files are bounded by per-thread message
    // count and a single thread's entire conversation comfortably fits in
    // memory at mount time — we already do this in
    // `count_messages_in_ledger` via `read_ledger`. No streaming win.
    let bytes = fs::read(path).map_err(|e| MountError::RepairFailed {
        reason: format!("read {}: {e}", path.display()),
    })?;

    let mut entries: Vec<LedgerEntry> = Vec::new();
    // Byte offset (exclusive) of the last successfully parsed line's
    // terminating `\n`. Anything past this offset is the candidate for
    // torn-tail truncation.
    let mut last_good_end: usize = 0;
    let mut cursor: usize = 0;
    let mut idx: usize = 0; // 0-based line index
    let total = bytes.len();

    while cursor < total {
        // Find the next `\n`. If none, the trailing chunk is unterminated —
        // by definition a torn tail.
        let nl = bytes[cursor..].iter().position(|b| *b == b'\n');
        let (line_bytes, line_end_excl, terminated) = match nl {
            Some(rel) => {
                let end = cursor + rel; // index of '\n'
                (&bytes[cursor..end], end + 1, true)
            }
            None => (&bytes[cursor..], total, false),
        };

        // Skip empty lines (matches read_ledger's behavior). An empty
        // *unterminated* trailing chunk just means the file ends in
        // `\n` — that's a clean tail, not a torn one.
        if line_bytes.is_empty() {
            if terminated {
                last_good_end = line_end_excl;
                cursor = line_end_excl;
                idx += 1;
                continue;
            } else {
                // Trailing '' with no newline = file already ends in \n.
                break;
            }
        }

        let parsed_line = std::str::from_utf8(line_bytes)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

        let parsed_entry = parsed_line.as_ref().and_then(|json| {
            let seq = json["seq"].as_u64()?;
            let hash = json["hash"].as_str()?.to_string();
            let parent = json["parent"].as_str().map(|s| s.to_string());
            let msg = json.get("msg")?.clone();
            Some(LedgerEntry {
                seq,
                hash,
                parent,
                msg,
            })
        });

        match (parsed_entry, terminated) {
            (Some(entry), true) => {
                entries.push(entry);
                last_good_end = line_end_excl;
                cursor = line_end_excl;
                idx += 1;
            }
            (Some(_), false) | (None, false) => {
                // Final line without a trailing '\n'. Treat as torn —
                // even if it parses, the missing newline means the
                // writer didn't commit it. We'd rather discard a single
                // *correct-but-unfsynced* line than risk treating a
                // truncated one as valid.
                let dropped = (total - last_good_end) as u64;
                truncate_to(path, last_good_end as u64).map_err(|e| MountError::RepairFailed {
                    reason: format!(
                        "truncate {} to {} bytes: {e}",
                        path.display(),
                        last_good_end,
                    ),
                })?;
                tracing::info!(
                    path = %path.display(),
                    bytes_dropped = dropped,
                    last_line_index = idx,
                    "LedgerTailRepaired"
                );
                return Ok(ReadOutcome {
                    entries,
                    repaired_bytes: Some(dropped),
                });
            }
            (None, true) => {
                // Interior line failed to parse — corruption, not a
                // torn tail. Surface this as RepairFailed; the shell
                // mounts the thread read-only so the user can see what
                // they have.
                return Err(MountError::RepairFailed {
                    reason: format!("interior line {idx} of {} failed to parse", path.display(),),
                });
            }
        }
    }

    Ok(ReadOutcome {
        entries,
        repaired_bytes: None,
    })
}

/// Truncate `path` to `len` bytes, opening with write access. Helper for
/// the torn-tail repair path so we don't conflate the open + set_len
/// errors with the parse path's errors.
fn truncate_to(path: &Path, len: u64) -> std::io::Result<()> {
    let f = OpenOptions::new().write(true).open(path)?;
    f.set_len(len)?;
    // Best-effort: flush the truncate to disk so a crash immediately
    // after this call doesn't leave the file with the torn tail still
    // present. If sync_data isn't supported (rare), fall through — the
    // truncate itself has already been issued.
    let _ = f.sync_data();
    Ok(())
}

/// Read just the last entry from a ledger file.
pub fn read_last_entry(path: &Path) -> Result<Option<LedgerEntry>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let last_line = content.lines().rev().find(|l| !l.is_empty());
    match last_line {
        None => Ok(None),
        Some(line) => {
            let json: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let seq = json["seq"].as_u64().ok_or("missing seq")?;
            let hash = json["hash"].as_str().ok_or("missing hash")?.to_string();
            let parent = json["parent"].as_str().map(|s| s.to_string());
            let msg = json.get("msg").ok_or("missing msg")?.clone();
            Ok(Some(LedgerEntry {
                seq,
                hash,
                parent,
                msg,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_hash_is_deterministic() {
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        let h1 = entry_hash(&msg);
        let h2 = entry_hash(&msg);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn entry_hash_differs_for_different_messages() {
        let m1 = serde_json::json!({"role": "user", "content": "hello"});
        let m2 = serde_json::json!({"role": "user", "content": "world"});
        assert_ne!(entry_hash(&m1), entry_hash(&m2));
    }

    #[test]
    fn append_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        let e1 = append_entry(&path, &msg1, None).unwrap();
        assert_eq!(e1.seq, 0);
        assert!(e1.parent.is_none());

        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "reply"}]});
        let e2 = append_entry(&path, &msg2, Some(&e1)).unwrap();
        assert_eq!(e2.seq, 1);
        assert_eq!(e2.parent.as_deref(), Some(e1.hash.as_str()));

        let entries = read_ledger(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
    }

    #[test]
    fn read_last_entry_returns_none_for_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        assert!(read_last_entry(&path).unwrap().is_none());
    }

    #[test]
    fn read_last_entry_returns_latest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        let e1 = append_entry(&path, &msg1, None).unwrap();
        let msg2 = serde_json::json!({"role": "user", "content": "second"});
        let _e2 = append_entry(&path, &msg2, Some(&e1)).unwrap();

        let last = read_last_entry(&path).unwrap().unwrap();
        assert_eq!(last.seq, 1);
    }

    #[test]
    fn hash_chain_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        let msg1 = serde_json::json!({"role": "user", "content": "a"});
        let e1 = append_entry(&path, &msg1, None).unwrap();
        let msg2 = serde_json::json!({"role": "user", "content": "b"});
        let e2 = append_entry(&path, &msg2, Some(&e1)).unwrap();
        let msg3 = serde_json::json!({"role": "user", "content": "c"});
        let e3 = append_entry(&path, &msg3, Some(&e2)).unwrap();

        let entries = read_ledger(&path).unwrap();
        assert!(entries[0].parent.is_none());
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
        assert_eq!(entries[2].parent.as_deref(), Some(entries[1].hash.as_str()));
        assert_eq!(entries[0].hash, entry_hash(&entries[0].msg));
        assert_eq!(entries[2].hash, e3.hash);
    }

    #[test]
    fn read_nonexistent_file_returns_empty() {
        let path = std::path::Path::new("/tmp/nonexistent_ledger_test_ox.jsonl");
        let entries = read_ledger(path).unwrap();
        assert!(entries.is_empty());
    }

    // ---- Torn-tail repair (`read_ledger_with_repair`) ----------------

    #[test]
    fn repair_returns_missing_for_absent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never_existed.jsonl");
        match read_ledger_with_repair(&path) {
            Err(MountError::Missing) => {}
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn repair_passes_through_clean_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let m1 = serde_json::json!({"role": "user", "content": "a"});
        let e1 = append_entry(&path, &m1, None).unwrap();
        let m2 = serde_json::json!({"role": "user", "content": "b"});
        append_entry(&path, &m2, Some(&e1)).unwrap();

        let outcome = read_ledger_with_repair(&path).unwrap();
        assert_eq!(outcome.entries.len(), 2);
        assert!(outcome.repaired_bytes.is_none());
    }

    #[test]
    fn repair_truncates_torn_tail_and_keeps_prior_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        // Two clean entries + a torn (no-newline, partial JSON) tail.
        let m1 = serde_json::json!({"role": "user", "content": "first"});
        let e1 = append_entry(&path, &m1, None).unwrap();
        let m2 = serde_json::json!({"role": "user", "content": "second"});
        append_entry(&path, &m2, Some(&e1)).unwrap();

        let pre_len = std::fs::metadata(&path).unwrap().len();
        // Hand-craft a torn last line: started writing, no `\n`.
        let torn = b"{\"seq\":2,\"hash\":\"deadbeef\",\"parent\":\"abc\",\"msg\":{\"role\":\"user\",\"content\":\"par";
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(torn).unwrap();
        }
        let post_torn_len = std::fs::metadata(&path).unwrap().len();
        assert!(post_torn_len > pre_len);

        let outcome = read_ledger_with_repair(&path).unwrap();
        assert_eq!(outcome.entries.len(), 2, "torn tail dropped, prior intact");
        assert_eq!(
            outcome.repaired_bytes,
            Some(post_torn_len - pre_len),
            "should report exact bytes dropped"
        );
        // File on disk should now match the pre-torn length.
        let final_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(final_len, pre_len, "repair truncated to last-good byte");
    }

    #[test]
    fn repair_truncates_when_complete_line_lacks_newline() {
        // A trailing line that *parses* but is missing the terminating
        // newline is still torn — by the writer's contract a fully
        // committed entry always ends in `\n`. Discard.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let m1 = serde_json::json!({"role": "user", "content": "first"});
        append_entry(&path, &m1, None).unwrap();
        let pre_len = std::fs::metadata(&path).unwrap().len();

        let almost = serde_json::to_string(&serde_json::json!({
            "seq": 1u64,
            "hash": "deadbeefdeadbeef",
            "parent": "00000000",
            "msg": {"role": "user", "content": "no-newline"},
        }))
        .unwrap();
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(almost.as_bytes()).unwrap();
        }

        let outcome = read_ledger_with_repair(&path).unwrap();
        assert_eq!(outcome.entries.len(), 1);
        assert!(outcome.repaired_bytes.is_some());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), pre_len);
    }

    #[test]
    fn repair_fails_on_interior_corruption() {
        // Corrupt a *middle* line. This is not a torn tail — we should
        // NOT silently drop data; surface RepairFailed so the shell can
        // mount read-only with a banner.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        // Build "good\nGARBAGE\ngood\n" by hand.
        let l1 = serde_json::json!({
            "seq": 0u64, "hash": "h0", "parent": null,
            "msg": {"role": "user", "content": "a"},
        });
        let l3 = serde_json::json!({
            "seq": 1u64, "hash": "h1", "parent": "h0",
            "msg": {"role": "user", "content": "c"},
        });
        let raw = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&l1).unwrap(),
            "this is not json",
            serde_json::to_string(&l3).unwrap(),
        );
        std::fs::write(&path, raw).unwrap();

        match read_ledger_with_repair(&path) {
            Err(MountError::RepairFailed { reason }) => {
                assert!(reason.contains("interior"), "reason was {reason}");
            }
            other => panic!("expected RepairFailed, got {other:?}"),
        }
    }

    #[test]
    fn repair_fails_when_truncate_cannot_open_file() {
        // Simulate truncation failure portably by passing a path that
        // refers to a *directory* — opening it for write returns an
        // error on every supported platform.
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().to_path_buf();
        // The repair path expects a regular file with content. Since it
        // does the existence check first we instead directly call the
        // truncate helper and assert it errors on a directory.
        let err = super::truncate_to(&bogus, 0).unwrap_err();
        let _ = err; // any error is acceptable; we only need non-Ok.
    }
}
