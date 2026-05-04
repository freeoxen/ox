//! Settings startup bootstrap — index entries, first-run cursor, legacy log.
//!
//! Called from `main.rs` after broker setup + subscription registration,
//! before the event loop starts. Each function is independently safe to
//! call: idempotent rewrites, conservative no-ops on conflict, and the
//! legacy detector is a pure filesystem read.

use std::path::Path as FsPath;

use ox_broker::ClientHandle;
use ox_path::oxpath;
use ox_types::{BadgeSource, SettingsIndexEntry};
use structfs_core_store::{Error as StoreError, Record};

use super::commands::navigation::path_to_value;

/// Write the day-one settings index entries (Accounts and Models) per
/// spec §6.1. Idempotent — overwrites whatever's there. Called once per
/// launch.
pub async fn populate_index_entries(client: &ClientHandle) -> Result<(), StoreError> {
    let accounts_entry = SettingsIndexEntry {
        id: "accounts".to_string(),
        label: "Accounts".to_string(),
        description: "Manage accounts and API keys.".to_string(),
        target_cursor: oxpath!("settings", "accounts"),
        badge: BadgeSource::SubtreeCount(oxpath!("config", "gate", "accounts")),
    };
    let models_entry = SettingsIndexEntry {
        id: "models".to_string(),
        label: "Models".to_string(),
        description: "Browse model catalogs and tag the bootstrap model.".to_string(),
        target_cursor: oxpath!("settings", "models"),
        badge: BadgeSource::BootstrapReference,
    };
    client
        .write_typed(
            &oxpath!("settings", "index", "entries", "accounts"),
            &accounts_entry,
        )
        .await?;
    client
        .write_typed(
            &oxpath!("settings", "index", "entries", "models"),
            &models_entry,
        )
        .await?;
    Ok(())
}

/// First-run hook: when no accounts exist and no settings cursor is set,
/// land the user on the new-account overlay so they have a clear next
/// step. Returns `true` when the hook fired, `false` otherwise.
pub async fn maybe_first_run_cursor(client: &ClientHandle) -> Result<bool, StoreError> {
    let cursor_path = oxpath!("ui", "settings", "cursor");
    if client.read(&cursor_path).await?.is_some() {
        return Ok(false);
    }
    // The accounts subtree is keyed by name (`accounts/{name}/...`); a
    // direct read at the prefix returns None on `LocalConfig` since it's
    // an exact-match store. Use `read_subtree` (which knows to walk the
    // mount root and filter) — same path the snapshot fetcher uses.
    let subtree = client
        .read_subtree(&oxpath!("config", "gate", "accounts"))
        .await
        .unwrap_or_default();
    if !subtree.is_empty() {
        return Ok(false);
    }
    let first_cursor = oxpath!("settings", "accounts", "_new");
    let value = path_to_value(&first_cursor);
    client.write(&cursor_path, Record::parsed(value)).await?;
    Ok(true)
}

/// Inspect the on-disk TOML for legacy schema sections that the new
/// settings code no longer reads. Returns identifiers of detected
/// blocks; empty Vec when none.
pub fn detect_legacy_settings(inbox_root: &FsPath) -> Vec<&'static str> {
    let toml_path = inbox_root.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&toml_path) else {
        return Vec::new();
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut detected = Vec::new();
    if let Some(toml::Value::Table(gate)) = table.get("gate") {
        if gate.contains_key("defaults") {
            detected.push("gate/defaults");
        }
        if let Some(toml::Value::Table(providers)) = gate.get("providers") {
            for (_name, prov) in providers {
                if let toml::Value::Table(prov) = prov {
                    if prov.contains_key("models") {
                        detected.push("gate/providers/*/models");
                        break;
                    }
                }
            }
        }
    }
    detected
}

/// Log once at startup if the on-disk config carries legacy settings
/// blocks. Prevents "where did my config go?" support questions.
pub fn log_legacy_settings_if_present(inbox_root: &FsPath) {
    let detected = detect_legacy_settings(inbox_root);
    if !detected.is_empty() {
        tracing::info!(
            paths = ?detected,
            "legacy settings detected; the new schema is in use — see \
             docs/superpowers/specs/2026-04-27-settings-screen-redesign.md §5.9 \
             for what changed."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_broker::BrokerStore;
    use ox_kernel::PathComponent;
    use std::time::Duration;
    use structfs_core_store::{Value, path};

    async fn fresh_broker() -> BrokerStore {
        let broker = BrokerStore::new(Duration::from_secs(2));
        let settings = ox_store_util::local_config::LocalConfig::new();
        let _rx = broker.mount(path!("settings"), settings).await;
        let config = ox_store_util::local_config::LocalConfig::new();
        let _rx2 = broker.mount(path!("config"), config).await;
        let ui = ox_store_util::local_config::LocalConfig::new();
        let _rx3 = broker.mount(path!("ui"), ui).await;
        // Keep the receivers alive in the test by leaking them — only this
        // fixture matters; the brokers Mount api drops them when the
        // BrokerStore is dropped, so we hold them for the test's lifetime
        // by binding into a tuple owned by the caller. For simplicity here
        // we accept the leak; tempfile-style integration tests use the
        // production broker_setup helpers.
        std::mem::forget(_rx);
        std::mem::forget(_rx2);
        std::mem::forget(_rx3);
        broker
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn populate_writes_both_entries() {
        let broker = fresh_broker().await;
        let client = broker.client();
        populate_index_entries(&client).await.unwrap();
        let accounts: SettingsIndexEntry = client
            .read_typed(&oxpath!("settings", "index", "entries", "accounts"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(accounts.id, "accounts");
        assert_eq!(accounts.target_cursor, oxpath!("settings", "accounts"));
        match accounts.badge {
            BadgeSource::SubtreeCount(p) => {
                assert_eq!(p, oxpath!("config", "gate", "accounts"))
            }
            other => panic!("unexpected badge: {:?}", other),
        }
        let models: SettingsIndexEntry = client
            .read_typed(&oxpath!("settings", "index", "entries", "models"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(models.id, "models");
        assert!(matches!(models.badge, BadgeSource::BootstrapReference));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn first_run_writes_cursor_when_accounts_empty() {
        let broker = fresh_broker().await;
        let client = broker.client();
        let fired = maybe_first_run_cursor(&client).await.unwrap();
        assert!(fired);
        let cursor = client
            .read(&oxpath!("ui", "settings", "cursor"))
            .await
            .unwrap()
            .expect("cursor written");
        // Decode via the same Value::Array shape path_to_value uses.
        match cursor.as_value().unwrap() {
            Value::Array(items) => {
                let comps: Vec<&str> = items
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(comps, vec!["settings", "accounts", "_new"]);
            }
            other => panic!("unexpected cursor shape: {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn first_run_no_op_when_cursor_already_set() {
        let broker = fresh_broker().await;
        let client = broker.client();
        // Pre-write a cursor to settings/index.
        let preset = oxpath!("settings", "index");
        client
            .write(
                &oxpath!("ui", "settings", "cursor"),
                Record::parsed(path_to_value(&preset)),
            )
            .await
            .unwrap();
        let fired = maybe_first_run_cursor(&client).await.unwrap();
        assert!(!fired);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn first_run_no_op_when_accounts_present() {
        let broker = fresh_broker().await;
        let client = broker.client();
        // Pre-write an account so the accounts subtree is non-empty.
        let name = PathComponent::try_new("anthropic_personal").unwrap();
        client
            .write(
                &oxpath!("config", "gate", "accounts", name, "provider"),
                Record::parsed(Value::String("anthropic".into())),
            )
            .await
            .unwrap();
        let fired = maybe_first_run_cursor(&client).await.unwrap();
        assert!(!fired);
        let cursor = client
            .read(&oxpath!("ui", "settings", "cursor"))
            .await
            .unwrap();
        assert!(cursor.is_none());
    }

    #[test]
    fn detect_legacy_returns_empty_for_clean_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gate.accounts.anthropic]\nprovider = \"anthropic\"\n",
        )
        .unwrap();
        assert!(detect_legacy_settings(dir.path()).is_empty());
    }

    #[test]
    fn detect_legacy_finds_gate_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gate.defaults]\naccount = \"anthropic\"\n",
        )
        .unwrap();
        assert_eq!(detect_legacy_settings(dir.path()), vec!["gate/defaults"]);
    }

    #[test]
    fn detect_legacy_finds_provider_models() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gate.providers.openai.models.gpt-4o]\nfoo = 1\n",
        )
        .unwrap();
        assert_eq!(
            detect_legacy_settings(dir.path()),
            vec!["gate/providers/*/models"]
        );
    }

    #[test]
    fn detect_legacy_returns_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_legacy_settings(dir.path()).is_empty());
    }
}
