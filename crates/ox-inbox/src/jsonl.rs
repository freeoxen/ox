use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use structfs_core_store::Error as StoreError;

pub fn append(path: &Path, line: &str) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| StoreError::store("InboxStore", "jsonl::append", e.to_string()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| StoreError::store("InboxStore", "jsonl::append", e.to_string()))?;
    writeln!(file, "{}", line)
        .map_err(|e| StoreError::store("InboxStore", "jsonl::append", e.to_string()))?;
    Ok(())
}

#[allow(dead_code)]
pub fn read_all(path: &Path) -> Result<Vec<String>, StoreError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path)
        .map_err(|e| StoreError::store("InboxStore", "jsonl::read_all", e.to_string()))?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .collect::<Result<_, _>>()
        .map_err(|e| StoreError::store("InboxStore", "jsonl::read_all", e.to_string()))?;
    Ok(lines)
}
