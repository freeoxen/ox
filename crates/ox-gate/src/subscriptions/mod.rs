//! Day-one broker subscription handlers for the settings screen.
//!
//! Each module here defines one `Subscription` impl that watches a path
//! pattern under `config/gate/accounts/` (or `config/save`) and reacts
//! by writing typed status / lifecycle records and / or spawning network
//! calls via [`crate::transport::Transport`].
//!
//! Wire them all up at startup with [`register_all`].

pub mod account_delete;
pub mod account_test;
pub mod catalog_refresh;
pub mod config_save;
pub mod util;

use std::sync::Arc;

use ox_broker::BrokerStore;

use crate::transport::Transport;

/// Register every day-one subscription on the broker.
///
/// Order matters when more than one subscription would match the same
/// write (the dispatcher fires them in registration order, and the
/// settings design relies on the create / delete pair landing before
/// the test / refresh handlers are even reachable). The order here is:
///
/// 1. `account_test` — instance-segment trigger, fires on `…/test_now`
/// 2. `catalog_refresh` — instance-segment trigger, fires on `…/refresh_now`
/// 3. `account_delete_cleanup` — reactive observer, fires on null
///    writes to `config/gate/accounts/<name>` (account-record depth)
/// 4. `config_save` — exact trigger, fires on `config/save`
///
/// Today the patterns are mutually exclusive (different suffixes /
/// exact paths), so order is informational; pinning it here makes any
/// future overlap that pulls one of these off the disjoint list a
/// deliberate choice.
pub fn register_all(broker: &BrokerStore, transport: Arc<dyn Transport>) {
    broker.register_subscription(Arc::new(account_test::AccountTestSubscription::new(
        transport.clone(),
    )));
    broker.register_subscription(Arc::new(catalog_refresh::CatalogRefreshSubscription::new(
        transport,
    )));
    broker.register_subscription(Arc::new(
        account_delete::AccountDeleteCleanupSubscription::new(),
    ));
    broker.register_subscription(Arc::new(config_save::ConfigSaveSubscription::new()));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ox_broker::BrokerStore;
    use ox_path::oxpath;
    use structfs_core_store::{Record, Value};

    use crate::subscriptions::register_all;
    use crate::transport::HttpTransport;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn register_all_does_not_panic() {
        let broker = BrokerStore::new(Duration::from_secs(2));
        let transport = Arc::new(HttpTransport);
        register_all(&broker, transport);
        // No assertion needed — the test just exercises the wiring.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_save_subscription_fires_without_error() {
        // Mount a memory backing for the `config` mount so the write
        // resolves; the subscription is the read-only observer here.
        use std::collections::BTreeMap;
        use structfs_core_store::{Error as StoreError, Path, Reader, Writer};

        struct MemStore(BTreeMap<String, Value>);
        impl Reader for MemStore {
            fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
                Ok(self
                    .0
                    .get(&from.to_string())
                    .map(|v| Record::parsed(v.clone())))
            }
        }
        impl Writer for MemStore {
            fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
                if let Some(v) = data.as_value() {
                    self.0.insert(to.to_string(), v.clone());
                }
                Ok(to.clone())
            }
        }

        let broker = BrokerStore::new(Duration::from_secs(2));
        let _h = broker
            .mount(oxpath!("config"), MemStore(BTreeMap::new()))
            .await;
        register_all(&broker, Arc::new(HttpTransport));

        // Write to config/save — the ConfigSaveSubscription is exact-matched
        // and its handler is a no-op. The write itself must succeed.
        let client = broker.client();
        client
            .write(&oxpath!("config", "save"), Record::parsed(Value::Null))
            .await
            .expect("config/save write should succeed");
    }
}
