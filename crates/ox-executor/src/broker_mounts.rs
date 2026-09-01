//! Broker mounts required by every native execution host.

use std::path::PathBuf;

use ox_broker::BrokerStore;
use ox_inbox::InboxStore;
use structfs_core_store::{Reader, Writer, path};

/// Mount the durable/global stores and per-thread registry shared by the
/// interactive CLI and headless workers.
///
/// Config and secret stores are supplied by the surface because their file
/// backing and initial values are presentation/configuration concerns. Their
/// namespace paths and the execution registry are common.
pub async fn mount_execution_stores<C, S>(
    broker: &BrokerStore,
    inbox: InboxStore,
    inbox_root: PathBuf,
    config: C,
    secrets: S,
) -> Vec<tokio::task::JoinHandle<()>>
where
    C: Reader + Writer + Send + 'static,
    S: Reader + Writer + Send + 'static,
{
    let mut servers = Vec::with_capacity(4);
    servers.push(broker.mount(path!("inbox"), inbox).await);
    servers.push(broker.mount(path!("config"), config).await);
    servers.push(broker.mount(path!("secret"), secrets).await);

    let mut registry = crate::ThreadRegistry::new(inbox_root);
    registry.set_broker_client(broker.client());
    servers.push(broker.mount_async(path!("threads"), registry).await);
    servers
}
