//! Pins that a broker `read(non_leaf_path)` routes through to the mount
//! that owns the prefix and returns whatever the mount produces. After
//! the read-at-prefix-returns-children-Map convention propagated into
//! LocalConfig, a non-leaf read against a LocalConfig mount yields a
//! `Value::Map` of immediate children — and the broker must not filter,
//! short-circuit, or otherwise interpret that result on the substrate
//! side. The Path is the API; routing is faithful.

use std::time::Duration;

use ox_broker::BrokerStore;
use ox_store_util::local_config::LocalConfig;
use structfs_core_store::{Record, Value, path};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_read_at_non_leaf_returns_children_map_from_mount() {
    let broker = BrokerStore::new(Duration::from_secs(5));
    let _mount = broker.mount(path!("settings"), LocalConfig::new()).await;
    let client = broker.client();

    client
        .write(
            &path!("settings/index/entries/accounts"),
            Record::parsed(Value::String("acc".into())),
        )
        .await
        .expect("write accounts");
    client
        .write(
            &path!("settings/index/entries/models"),
            Record::parsed(Value::String("mod".into())),
        )
        .await
        .expect("write models");

    let rec = client
        .read(&path!("settings/index/entries"))
        .await
        .expect("broker read")
        .expect("non-leaf returns Some");
    let value = rec.as_value().expect("non-leaf record has value");
    let map = match value {
        Value::Map(m) => m.clone(),
        other => panic!("expected Value::Map; got {other:?}"),
    };
    assert!(
        map.contains_key("accounts"),
        "accounts missing; map={map:?}"
    );
    assert!(map.contains_key("models"), "models missing; map={map:?}");
    assert_eq!(
        map.len(),
        2,
        "expected exactly two immediate children; map={map:?}"
    );
}
