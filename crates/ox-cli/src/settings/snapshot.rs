//! Pre-render snapshot for the settings screen.
//!
//! Renderers are pure `&dyn Reader -> View` functions. Before drawing,
//! the event loop materializes a `SettingsSnapshot` — an in-memory
//! `Reader` populated from the six namespaces the settings UI reads.
//!
//! Why a snapshot? The renderer pipeline reads many paths during a
//! single render frame; we don't want N async round-trips through the
//! broker per frame. The snapshot is built once per frame, asynchronously,
//! then handed to the (synchronous) renderer pipeline.

use ox_broker::ClientHandle;
use ox_store_util::local_config::LocalConfig;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value};

/// In-memory `Reader` populated from a fixed set of broker prefixes.
///
/// Internally a `LocalConfig`. Insert during construction (via
/// `fetch_settings_view_state` or `insert` for tests); read via the
/// `Reader` trait once handed to the renderer pipeline.
pub struct SettingsSnapshot {
    inner: LocalConfig,
}

impl SettingsSnapshot {
    /// Create an empty snapshot.
    pub fn empty() -> Self {
        Self {
            inner: LocalConfig::new(),
        }
    }

    /// Insert one (path, value) pair. Used by `fetch_settings_view_state`
    /// during construction; also handy for renderer tests building a
    /// snapshot in-line.
    pub(crate) fn insert(&mut self, path: &Path, value: Value) {
        self.inner.set(&path.to_string(), value);
    }
}

impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}

impl Reader for SettingsSnapshot {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.inner.read(from)
    }
}

/// The prefixes the settings UI reads (per spec §6).
///
/// `secret/keys` is included so renderers can drive the per-account
/// "key present?" indicator without an out-of-band sync read (the broker
/// resolves `secret/*` from the same `JsonFileBacking` mounts as `config/*`).
/// Renderers themselves never deserialize the wrapped key body — they only
/// observe presence — but `read_subtree` returns the typed `ApiKey` value
/// and the snapshot stores it verbatim.
const PREFIXES: &[&str] = &[
    "config/gate/accounts",
    "config/gate/providers",
    "config/completions",
    "ui/settings",
    "ui/global",
    "settings/index/entries",
    "secret/keys",
];

/// Build a snapshot by walking every prefix the settings UI reads.
///
/// Each prefix is enumerated via `ClientHandle::read_subtree` and every
/// resulting `(path, record)` is inserted into the snapshot's inner store.
/// On a per-prefix read error we log and skip — partial snapshots are
/// preferable to a panic mid-frame, and the renderer's empty-state code
/// will surface the missing data appropriately.
pub async fn fetch_settings_view_state(client: &ClientHandle) -> SettingsSnapshot {
    let mut snap = SettingsSnapshot::empty();
    for prefix_str in PREFIXES {
        let prefix = match Path::parse(prefix_str) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(prefix = prefix_str, error = %e, "snapshot: bad prefix literal");
                continue;
            }
        };
        match client.read_subtree(&prefix).await {
            Ok(entries) => {
                for (path, record) in entries {
                    if let Some(value) = record.as_value() {
                        snap.insert(&path, value.clone());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(prefix = prefix_str, error = %e, "snapshot: prefix read failed");
            }
        }
    }
    snap
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::time::Duration;

    use ox_broker::BrokerStore;
    use ox_gate::CompletionRole;
    use ox_path::oxpath;
    use ox_ui::config_store::ConfigStore;

    /// Mount a `ConfigStore` at `config` and return broker + client.
    async fn broker_with_config() -> (BrokerStore, ClientHandle) {
        let broker = BrokerStore::new(Duration::from_secs(5));
        let store = ConfigStore::new(BTreeMap::new());
        let _h = broker.mount(oxpath!("config"), store).await;
        let client = broker.client();
        (broker, client)
    }

    /// The headline smoke test from the plan: write a typed `CompletionRole`
    /// to the broker, build a snapshot, deserialize the same value back via
    /// `Reader::read`. End-to-end validates that the snapshot preserves the
    /// exact `Value` shape across the broker round-trip.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn smoke_test_round_trips_completion_role() {
        let (_broker, client) = broker_with_config().await;

        let role = CompletionRole {
            account: "anthropic-personal".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
        };
        client
            .write_typed(&oxpath!("config", "completions", "primary"), &role)
            .await
            .expect("write_typed");

        let mut snap = fetch_settings_view_state(&client).await;
        let record = snap
            .read(&oxpath!("config", "completions", "primary"))
            .expect("read")
            .expect("record present");
        let value = record.as_value().expect("parsed value").clone();
        let read_back: CompletionRole =
            structfs_serde_store::from_value(value).expect("from_value");
        assert_eq!(read_back, role);
    }

    /// A prefix with no data populates nothing — and other prefixes still
    /// work. Verifies the per-prefix error isolation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn empty_prefixes_yield_empty_subtree() {
        let (_broker, client) = broker_with_config().await;
        // No writes at all — every prefix is empty.
        let mut snap = fetch_settings_view_state(&client).await;
        // Reading anything specific returns None.
        assert!(
            snap.read(&oxpath!("config", "gate", "accounts", "anything"))
                .unwrap()
                .is_none()
        );
    }

    /// Multiple keys under one prefix all land in the snapshot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_account_entries_round_trip() {
        let (_broker, client) = broker_with_config().await;

        client
            .write_typed(
                &oxpath!("config", "gate", "accounts", "alpha", "endpoint"),
                &"https://api.alpha.test".to_string(),
            )
            .await
            .unwrap();
        client
            .write_typed(
                &oxpath!("config", "gate", "accounts", "beta", "endpoint"),
                &"https://api.beta.test".to_string(),
            )
            .await
            .unwrap();

        let mut snap = fetch_settings_view_state(&client).await;
        let alpha = snap
            .read(&oxpath!("config", "gate", "accounts", "alpha", "endpoint"))
            .unwrap()
            .unwrap();
        let beta = snap
            .read(&oxpath!("config", "gate", "accounts", "beta", "endpoint"))
            .unwrap()
            .unwrap();
        assert_eq!(
            alpha.as_value().unwrap(),
            &Value::String("https://api.alpha.test".into())
        );
        assert_eq!(
            beta.as_value().unwrap(),
            &Value::String("https://api.beta.test".into())
        );
    }

    /// `SettingsSnapshot` honors the `Reader` contract for absent paths.
    #[test]
    fn empty_snapshot_returns_none_for_unknown_path() {
        let mut snap = SettingsSnapshot::empty();
        let result = snap.read(&oxpath!("nope")).unwrap();
        assert!(result.is_none());
    }

    /// `insert` then `read` preserves the value verbatim.
    #[test]
    fn insert_then_read_preserves_value() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "global", "mode"),
            Value::String("normal".into()),
        );
        let record = snap
            .read(&oxpath!("ui", "global", "mode"))
            .unwrap()
            .unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::String("normal".into()));
    }
}
