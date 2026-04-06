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
}
