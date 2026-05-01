//! Shared helpers for the day-one subscription handlers.
//!
//! Every handler in this directory needs to:
//! - extract the instance segment from a `PrefixSuffix` triggered path
//! - build paths to the four per-account locations (account map, secret
//!   key, validation diagnostics, status records)
//! - read typed records out of a live broker reader and skip the
//!   handler when something's missing or malformed
//! - serialize a typed value back into a `Write`
//!
//! Centralizing these here keeps `account_test.rs`, `catalog_refresh.rs`,
//! `account_delete.rs`, `account_create.rs`, and `config_save.rs`
//! focused on the subscription's domain logic instead of path plumbing.

use std::time::{SystemTime, UNIX_EPOCH};

use ox_kernel::PathComponent;
use ox_path::oxpath;
use ox_types::subscription::Write;
use structfs_core_store::{Path, Reader, Record};
use structfs_serde_store::{from_value, to_value};

// ---------------------------------------------------------------------------
// Path construction
// ---------------------------------------------------------------------------

/// Extract the instance segment from a `PrefixSuffix`-matched path.
///
/// Returns `Some(name)` when `change_path = prefix / <one component> /
/// suffix`. Returns `None` when the shape doesn't match (e.g. multi-segment
/// instances — those are valid per the broker's `PathPattern::PrefixSuffix`
/// matcher but the day-one subscriptions all use single-segment account
/// names, so anything else is a no-op for them).
pub fn instance_segment(change_path: &Path, prefix: &Path, suffix: &Path) -> Option<String> {
    // Need at least one segment between prefix and suffix.
    if change_path.len() != prefix.len() + 1 + suffix.len() {
        return None;
    }
    if !change_path.has_prefix(prefix) {
        return None;
    }
    let tail_start = change_path.len() - suffix.len();
    if change_path.components[tail_start..] != suffix.components[..] {
        return None;
    }
    Some(change_path.components[prefix.len()].clone())
}

/// `config/gate/accounts/{name}` — the account record path.
pub fn account_path(name: &str) -> Result<Path, String> {
    let comp = PathComponent::try_new(name).map_err(|e| e.to_string())?;
    Ok(oxpath!("config", "gate", "accounts", comp))
}

/// `config/gate/providers/{name}` — the per-account synthetic provider entry.
pub fn provider_path(name: &str) -> Result<Path, String> {
    let comp = PathComponent::try_new(name).map_err(|e| e.to_string())?;
    Ok(oxpath!("config", "gate", "providers", comp))
}

/// `secret/keys/{name}` — the API-key secret path.
pub fn secret_key_path(name: &str) -> Result<Path, String> {
    let comp = PathComponent::try_new(name).map_err(|e| e.to_string())?;
    Ok(oxpath!("secret", "keys", comp))
}

/// `config/gate/accounts/{name}/test_status` — the test-connection
/// lifecycle record.
pub fn test_status_path(name: &str) -> Result<Path, String> {
    Ok(account_path(name)?.join(&oxpath!("test_status")))
}

/// `config/gate/accounts/{name}/refresh_status` — the catalog-refresh
/// lifecycle record.
pub fn refresh_status_path(name: &str) -> Result<Path, String> {
    Ok(account_path(name)?.join(&oxpath!("refresh_status")))
}

/// `config/gate/accounts/{name}/models` — the per-account model catalog.
pub fn models_path(name: &str) -> Result<Path, String> {
    Ok(account_path(name)?.join(&oxpath!("models")))
}

/// `config/gate/accounts/{name}/validation` — per-field validation diagnostics.
pub fn validation_path(name: &str) -> Result<Path, String> {
    Ok(account_path(name)?.join(&oxpath!("validation")))
}

// ---------------------------------------------------------------------------
// Typed reads / writes against the live broker reader
// ---------------------------------------------------------------------------

/// Read a typed `T` from the live reader at `path`.
///
/// Returns `None` when the path is unset, the read fails, or
/// deserialization fails. Subscription handlers are total over their
/// input — a missing or corrupt record means "do nothing here," not
/// "panic the dispatcher."
pub fn read_typed_via_reader<T: serde::de::DeserializeOwned>(
    reader: &mut dyn Reader,
    path: &Path,
) -> Option<T> {
    reader
        .read(path)
        .ok()
        .flatten()
        .and_then(|r| r.as_value().cloned())
        .and_then(|v| from_value(v).ok())
}

/// Build a `Write` from a typed value. Falls back to `Value::Null` if
/// serialization somehow fails — the dispatcher won't panic on the
/// resulting record but the path will read as deleted, which is more
/// honest than crashing the handler.
pub fn write_typed<T: serde::Serialize>(path: &Path, value: &T) -> Write {
    let v = to_value(value).unwrap_or(structfs_core_store::Value::Null);
    Write {
        path: path.clone(),
        record: Record::parsed(v),
    }
}

/// Write a literal `Value::Null` at the given path — the canonical
/// "delete" shape for stores that follow the runtime-overlay convention.
pub fn null_write(path: Path) -> Write {
    Write {
        path,
        record: Record::parsed(structfs_core_store::Value::Null),
    }
}

/// Encode a `Path` as a `Value::Array` of `Value::String` segments — the
/// wire shape used by `ox_types::path_serde` and by the existing CLI
/// command helpers (see `crates/ox-cli/src/settings/commands/navigation.rs`).
/// Path itself doesn't implement `Serialize`, so callers writing `Path`
/// values into the broker go through this encoder.
pub fn path_to_value(p: &Path) -> structfs_core_store::Value {
    structfs_core_store::Value::Array(
        p.components
            .iter()
            .map(|c| structfs_core_store::Value::String(c.clone()))
            .collect(),
    )
}

/// Build a `Write` that puts the encoded `Path` at `at`. Pairs with
/// `path_to_value` for the cursor / target_cursor shape.
pub fn write_path(at: &Path, value: &Path) -> Write {
    Write {
        path: at.clone(),
        record: Record::parsed(path_to_value(value)),
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Epoch milliseconds. Subscription handlers stamp every status
/// transition with this so the UI can render "started 3s ago" without a
/// separate clock channel.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Test fixtures shared across the subscription tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(dead_code)] // some helpers (with_catalog, entries) are consumed by
                    // sibling subscription tests added in N4/N5/N6/N8.
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use ox_broker::async_store::BoxFuture;
    use ox_broker::subscription::{AsyncWriter, SpawnHandle};
    use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value};
    use tokio::task::AbortHandle;

    use crate::transport::{TestResult, Transport};
    use crate::{ModelInfo, ProviderConfig};

    // ---------- Mock spawn ----------
    //
    // ox-broker's MockSpawn is gated under `#[cfg(test)]` and unreachable
    // from here. We need a parallel implementation that records spawn
    // handles so the supersession tests can assert prior tasks were
    // aborted.

    /// Test `SpawnHandle` that records the spawned `AbortHandle`s.
    pub struct TestSpawn {
        handles: Mutex<Vec<AbortHandle>>,
    }

    impl Default for TestSpawn {
        fn default() -> Self {
            Self::new()
        }
    }

    impl TestSpawn {
        pub fn new() -> Self {
            Self {
                handles: Mutex::new(Vec::new()),
            }
        }

        /// Recorded handles, in spawn order.
        pub fn handles(&self) -> Vec<AbortHandle> {
            self.handles.lock().unwrap().clone()
        }
    }

    impl SpawnHandle for TestSpawn {
        fn spawn(&self, task: BoxFuture<()>) -> AbortHandle {
            let handle = tokio::spawn(task).abort_handle();
            self.handles.lock().unwrap().push(handle.clone());
            handle
        }
    }

    // ---------- Mock async writer ----------
    //
    // Records every write the spawned task makes. Tests assert the
    // expected status records landed by inspecting the recorded entries.

    #[derive(Clone, Default)]
    pub struct CapturingWriter {
        entries: Arc<Mutex<BTreeMap<String, Record>>>,
    }

    impl CapturingWriter {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn entries(&self) -> BTreeMap<String, Record> {
            self.entries.lock().unwrap().clone()
        }

        pub fn get(&self, path: &str) -> Option<Record> {
            self.entries.lock().unwrap().get(path).cloned()
        }

        pub fn typed<T: serde::de::DeserializeOwned>(&self, path: &str) -> Option<T> {
            let r = self.get(path)?;
            let v = r.as_value()?.clone();
            structfs_serde_store::from_value(v).ok()
        }
    }

    impl AsyncWriter for CapturingWriter {
        fn write(&self, path: Path, record: Record) -> BoxFuture<Result<Path, StoreError>> {
            let entries = self.entries.clone();
            Box::pin(async move {
                entries.lock().unwrap().insert(path.to_string(), record);
                Ok(path)
            })
        }
    }

    // ---------- Mock Reader ----------
    //
    // Backs the SubCtx's `snapshot` field. Owned by the test so it can
    // pre-populate AccountConfig / ProviderConfig / ApiKey records.

    pub struct InMemoryReader {
        pub data: BTreeMap<String, Value>,
    }

    impl InMemoryReader {
        pub fn new() -> Self {
            Self {
                data: BTreeMap::new(),
            }
        }

        pub fn set<T: serde::Serialize>(&mut self, path: &str, value: &T) {
            let v = structfs_serde_store::to_value(value).expect("serialize");
            self.data.insert(path.to_string(), v);
        }
    }

    impl Reader for InMemoryReader {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(self
                .data
                .get(&from.to_string())
                .map(|v| Record::parsed(v.clone())))
        }
    }

    // ---------- Mock Transport ----------
    //
    // Records calls so tests can assert the right account name reached
    // the network layer; configurable response so tests cover both
    // success and failure paths.

    #[derive(Clone)]
    pub struct MockTransport {
        pub test_response: Arc<Mutex<Result<TestResult, String>>>,
        pub catalog_response: Arc<Mutex<Result<Vec<ModelInfo>, String>>>,
        pub test_calls: Arc<Mutex<Vec<String>>>,
        pub catalog_calls: Arc<Mutex<Vec<String>>>,
    }

    impl Default for MockTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MockTransport {
        pub fn new() -> Self {
            Self {
                test_response: Arc::new(Mutex::new(Ok(("anthropic".to_string(), 42)))),
                catalog_response: Arc::new(Mutex::new(Ok(vec![]))),
                test_calls: Arc::new(Mutex::new(Vec::new())),
                catalog_calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn with_test_result(self, result: Result<TestResult, String>) -> Self {
            *self.test_response.lock().unwrap() = result;
            self
        }

        pub fn with_catalog(self, result: Result<Vec<ModelInfo>, String>) -> Self {
            *self.catalog_response.lock().unwrap() = result;
            self
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn test_connection(
            &self,
            account: &str,
            _provider: &ProviderConfig,
            _api_key: &str,
        ) -> Result<TestResult, String> {
            self.test_calls.lock().unwrap().push(account.to_string());
            self.test_response.lock().unwrap().clone()
        }

        async fn fetch_catalog(
            &self,
            account: &str,
            _provider: &ProviderConfig,
            _api_key: &str,
        ) -> Result<Vec<ModelInfo>, String> {
            self.catalog_calls.lock().unwrap().push(account.to_string());
            self.catalog_response.lock().unwrap().clone()
        }
    }

    // ---------- Helpers for assembling a SubCtx in tests ----------

    /// Pre-populate an InMemoryReader with a default Anthropic account
    /// shape (account record at `config/gate/accounts/{name}`, provider
    /// record at `config/gate/providers/{name}`, key at
    /// `secret/keys/{name}`). Tests that need other shapes call `.set`
    /// directly.
    pub fn populate_anthropic_account(reader: &mut InMemoryReader, name: &str, key: &str) {
        use crate::{AccountConfig, ApiKey};

        reader.set(
            &format!("config/gate/accounts/{name}"),
            &AccountConfig {
                provider: name.to_string(),
            },
        );
        reader.set(
            &format!("config/gate/providers/{name}"),
            &ProviderConfig::anthropic(),
        );
        reader.set(&format!("secret/keys/{name}"), &ApiKey::new(key));
    }
}
