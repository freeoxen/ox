//! Append-only usage ledger as a StructFS mount.
//!
//! Mounted at `gateway/usage` in the broker. Writes to `append` push one
//! `UsageRecord` to the backing (JSONL on disk in production, in-memory
//! in tests). Reads at root return the full ledger as `Vec<UsageRecord>`;
//! reads at `today` return an aggregated projection over the last 24h
//! window starting at midnight UTC.

use serde::{Deserialize, Serialize};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};
use structfs_serde_store::{from_value, to_value};

/// One completion's usage line. Appended to the ledger on terminal
/// status by `CompletionBrokerStore`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRecord {
    pub id: String,
    pub account: String,
    pub model_id: String,
    /// Inbound dialect the client sent ("anthropic" | "openai" | "ox").
    pub dialect: String,
    /// Resolved provider.dialect used for the upstream call.
    pub upstream_dialect: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    /// Best-effort cost estimate from `pricing::model_pricing`. None when
    /// the model isn't in the pricing table — better to show absence than
    /// to lie about cost.
    pub estimated_cost_usd: Option<f64>,
}

/// Read-projection over today's records (UTC-midnight boundary).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TodayProjection {
    pub count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
}

/// The usage-ledger store.
pub struct UsageStore {
    backing: Box<dyn ox_store_util::StoreBacking + Send + Sync>,
}

impl UsageStore {
    pub fn new(backing: Box<dyn ox_store_util::StoreBacking + Send + Sync>) -> Self {
        Self { backing }
    }

    fn append(&self, record: &UsageRecord) -> Result<(), StoreError> {
        let value = to_value(record)
            .map_err(|e| StoreError::store("usage", "append", e.to_string()))?;
        self.backing.append(&value)
    }

    fn load_all(&self) -> Result<Vec<UsageRecord>, StoreError> {
        let value = self.backing.load()?.unwrap_or(Value::Array(vec![]));
        let arr = match value {
            Value::Array(a) => a,
            _ => return Ok(vec![]),
        };
        Ok(arr.into_iter().filter_map(|v| from_value(v).ok()).collect())
    }
}

impl Reader for UsageStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from.is_empty() {
            let records = self.load_all()?;
            let value = to_value(&records)
                .map_err(|e| StoreError::store("usage", "read", e.to_string()))?;
            return Ok(Some(Record::parsed(value)));
        }
        match from[0].as_str() {
            "today" => {
                let records = self.load_all()?;
                let start_of_today_ms = start_of_today_ms();
                let total: TodayProjection = records
                    .iter()
                    .filter(|r| r.completed_at_ms >= start_of_today_ms)
                    .fold(TodayProjection::default(), |mut acc, r| {
                        acc.count += 1;
                        acc.input_tokens += r.input_tokens as u64;
                        acc.output_tokens += r.output_tokens as u64;
                        if let Some(c) = r.estimated_cost_usd {
                            acc.estimated_cost_usd += c;
                        }
                        acc
                    });
                let value = to_value(&total)
                    .map_err(|e| StoreError::store("usage", "read", e.to_string()))?;
                Ok(Some(Record::parsed(value)))
            }
            _ => Ok(None),
        }
    }
}

impl Writer for UsageStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if to.is_empty() || to[0].as_str() != "append" {
            return Err(StoreError::store(
                "usage",
                "write",
                "only 'append' path supported",
            ));
        }
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("usage", "write", "expected parsed record"))?;
        let record: UsageRecord = from_value(value.clone())
            .map_err(|e| StoreError::store("usage", "write", e.to_string()))?;
        self.append(&record)?;
        Ok(to.clone())
    }
}

fn start_of_today_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let day = 86_400_000u64;
    (now / day) * day
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_store_util::JsonlFileBacking;

    fn sample_record(id: &str) -> UsageRecord {
        UsageRecord {
            id: id.into(),
            account: "anthropic".into(),
            model_id: "claude-sonnet-4-20250514".into(),
            dialect: "anthropic".into(),
            upstream_dialect: "anthropic".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            started_at_ms: 1_000_000,
            completed_at_ms: 1_001_000,
            estimated_cost_usd: Some(0.0015),
        }
    }

    #[test]
    fn append_and_read_full_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        let backing = Box::new(JsonlFileBacking::new(&path).unwrap());
        let mut store = UsageStore::new(backing);

        let r = sample_record("a");
        let v = to_value(&r).unwrap();
        store
            .write(
                &structfs_core_store::path!("append"),
                Record::parsed(v),
            )
            .unwrap();

        let read = store
            .read(&structfs_core_store::path!(""))
            .unwrap()
            .unwrap();
        let value = read.as_value().unwrap();
        let records: Vec<UsageRecord> = from_value(value.clone()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "a");
    }

    #[test]
    fn write_rejects_non_append_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        let backing = Box::new(JsonlFileBacking::new(&path).unwrap());
        let mut store = UsageStore::new(backing);

        let r = sample_record("a");
        let v = to_value(&r).unwrap();
        let err = store
            .write(&structfs_core_store::path!("wrong"), Record::parsed(v))
            .unwrap_err();
        assert!(err.to_string().contains("append"));
    }

    #[test]
    fn read_unknown_path_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        let backing = Box::new(JsonlFileBacking::new(&path).unwrap());
        let mut store = UsageStore::new(backing);
        assert!(store
            .read(&structfs_core_store::path!("bogus"))
            .unwrap()
            .is_none());
    }
}
