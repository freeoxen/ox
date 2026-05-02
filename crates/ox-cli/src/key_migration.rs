//! One-shot migration of legacy on-disk API keys into the broker's secrets
//! namespace at `secret/keys/{name}: ApiKey`.
//!
//! Runs once at startup, before the event loop. Idempotent: skips entirely
//! when `secret/keys/*` is already populated (post-migration runs see the
//! `keys.json` file load up front via `ConfigStore::with_backing`). On a
//! fresh install with no files and no env vars set, this is a no-op.

use std::path::Path;
use structfs_core_store::{Path as StorePath, Record, Value};

/// Migrate legacy API keys into `secret/keys/{name}: ApiKey`.
///
/// Behaviour:
/// - If `secret/keys/*` is already populated, do nothing and return 0.
/// - Otherwise, scan `keys_dir/*.key` plus `OX_GATE__ACCOUNTS__{NAME}__KEY`
///   env vars (env beats file for the same name).
/// - For each non-empty key, write it through the broker as
///   `secret/keys/{name}: ApiKey`. Trigger `secret/save` once at the end
///   so the keys land in `keys.json` (chmod 0600); the legacy `*.key`
///   files are *not* deleted — losing a key file because of a broker
///   write failure would be unrecoverable.
///
/// Returns the number of keys migrated.
pub async fn migrate_legacy_keys(
    client: &ox_broker::ClientHandle,
    keys_dir: &Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    if secrets_keys_already_populated(client).await {
        tracing::debug!("secret/keys already populated — skipping legacy migration");
        return Ok(0);
    }

    let legacy = crate::config::read_legacy_key_sources(keys_dir);
    if legacy.is_empty() {
        return Ok(0);
    }

    let attempted = legacy.len();
    let mut migrated = 0usize;
    for (name, key) in &legacy {
        let name_comp = match ox_kernel::PathComponent::try_new(name.as_str()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(name, error = %e, "skipping legacy key with invalid account name");
                continue;
            }
        };
        let path = ox_path::oxpath!("secret", "keys", name_comp);
        match client
            .write_typed(&path, &ox_gate::ApiKey::new(key.clone()))
            .await
        {
            Ok(_) => migrated += 1,
            Err(e) => {
                tracing::warn!(name, error = %e, "failed to migrate legacy key");
            }
        }
    }

    if migrated > 0 {
        // Persist secrets to disk so the migration result survives the
        // process exit even if the user never opens the settings UI.
        // Surface a partial-failure (saved-to-disk) error so operators
        // notice when the broker accepted writes but the file flush
        // didn't land — silent `.ok()` here would leave the migration
        // counted as a success while the next launch re-migrates.
        let save_path = StorePath::parse("secret/save")
            .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
        if let Err(e) = client.write(&save_path, Record::parsed(Value::Null)).await {
            tracing::warn!(error = %e, "secret/save failed after legacy key migration");
        }
        tracing::info!(
            attempted,
            migrated,
            failed = attempted - migrated,
            "migrated {migrated}/{attempted} legacy key files into namespace"
        );
    } else if attempted > 0 {
        tracing::warn!(
            attempted,
            "legacy key migration found {attempted} candidate(s) but all writes failed"
        );
    }

    Ok(migrated)
}

/// `true` when the secrets store already has anything under `keys/`. Used
/// to make the migration idempotent.
async fn secrets_keys_already_populated(client: &ox_broker::ClientHandle) -> bool {
    let Ok(secret_path) = StorePath::parse("secret") else {
        return false;
    };
    let record = match client.read(&secret_path).await {
        Ok(Some(r)) => r,
        _ => return false,
    };
    let Some(Value::Map(m)) = record.as_value().cloned() else {
        return false;
    };
    m.keys().any(|k| k.starts_with("keys/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_path::oxpath;

    /// Build a broker with the same shape main.rs uses — `config/` mounted
    /// to a TOML backing under `inbox_root` and `secret/` mounted to a
    /// JSON backing.
    async fn test_broker(
        inbox_root: &std::path::Path,
    ) -> (ox_broker::ClientHandle, Vec<tokio::task::JoinHandle<()>>) {
        use structfs_core_store::path;

        let broker = ox_broker::BrokerStore::default();
        let mut servers = Vec::new();

        let toml_path = inbox_root.join("config.toml");
        let toml_backing = crate::toml_backing::TomlFileBacking::new(toml_path);
        let config = ox_ui::ConfigStore::with_backing(
            std::collections::BTreeMap::new(),
            Box::new(toml_backing),
        );
        servers.push(broker.mount(path!("config"), config).await);

        let keys_path = inbox_root.join("keys.json");
        let json_backing = crate::json_backing::JsonFileBacking::new(keys_path);
        let secrets = ox_ui::ConfigStore::with_backing(
            std::collections::BTreeMap::new(),
            Box::new(json_backing),
        );
        servers.push(broker.mount(path!("secret"), secrets).await);

        (broker.client(), servers)
    }

    #[tokio::test]
    async fn migrates_legacy_files_into_secret_keys() {
        let dir = tempfile::tempdir().unwrap();
        let inbox_root = dir.path();
        let keys_dir = inbox_root.join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        std::fs::write(keys_dir.join("anthropic.key"), "sk-ant-test\n").unwrap();
        std::fs::write(keys_dir.join("openai.key"), "sk-oai-test\n").unwrap();

        let (client, _servers) = test_broker(inbox_root).await;
        let n = migrate_legacy_keys(&client, &keys_dir).await.unwrap();
        assert_eq!(n, 2, "expected both legacy keys to migrate");

        // ApiKey lands at secret/keys/{name}.
        let comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        let key: Option<ox_gate::ApiKey> = client
            .read_typed(&oxpath!("secret", "keys", comp))
            .await
            .unwrap();
        assert_eq!(key.unwrap().expose(), "sk-ant-test");

        // keys.json was written with 0600 perms.
        let keys_json = inbox_root.join("keys.json");
        assert!(keys_json.exists(), "keys.json should exist after migration");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&keys_json).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn migration_is_idempotent_when_secret_keys_present() {
        let dir = tempfile::tempdir().unwrap();
        let inbox_root = dir.path();
        let keys_dir = inbox_root.join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        std::fs::write(keys_dir.join("anthropic.key"), "from-disk\n").unwrap();

        let (client, _servers) = test_broker(inbox_root).await;
        // Pre-seed the secrets namespace with a different key — the
        // migration must not overwrite it.
        let comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        client
            .write_typed(
                &oxpath!("secret", "keys", comp.clone()),
                &ox_gate::ApiKey::new("from-broker"),
            )
            .await
            .unwrap();

        let n = migrate_legacy_keys(&client, &keys_dir).await.unwrap();
        assert_eq!(n, 0, "must skip when secret/keys is already populated");

        let key: ox_gate::ApiKey = client
            .read_typed(&oxpath!("secret", "keys", comp))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.expose(), "from-broker");
    }

    #[tokio::test]
    async fn migration_with_no_legacy_files_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let inbox_root = dir.path();
        // No keys directory at all.
        let (client, _servers) = test_broker(inbox_root).await;
        let n = migrate_legacy_keys(&client, &inbox_root.join("keys"))
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}
