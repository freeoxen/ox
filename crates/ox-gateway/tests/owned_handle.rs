//! Allocation replies must survive caller cancellation and broker timeouts.
use ox_broker::{BrokerStore, async_store::BoxFuture};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use structfs_core_store::{DetachedReader, DetachedWriter, Error, Path, Record, Value, path};

struct DelayedHandles {
    live: Arc<AtomicUsize>,
    accepted: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl DetachedReader for DelayedHandles {
    fn read_detached(&mut self, _: &Path) -> BoxFuture<Result<Option<Record>, Error>> {
        Box::pin(async { Ok(None) })
    }
}
impl DetachedWriter for DelayedHandles {
    fn write_detached(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, Error>> {
        let live = self.live.clone();
        let accepted = self.accepted.clone();
        let release = self.release.clone();
        let to = to.clone();
        Box::pin(async move {
            if matches!(data.as_value(), Some(Value::Null)) {
                live.store(0, Ordering::SeqCst);
                return Ok(to);
            }
            live.store(1, Ordering::SeqCst);
            accepted.notify_one();
            release.notified().await;
            Ok(path!("outstanding/0"))
        })
    }
}
#[tokio::test]
async fn abandoned_open_retains_late_reply_and_cleans_handle() {
    let broker = BrokerStore::new(Duration::from_millis(5));
    let live = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    broker
        .mount_async(
            path!("wire"),
            DelayedHandles {
                live: live.clone(),
                accepted: accepted.clone(),
                release: release.clone(),
            },
        )
        .await;
    let task = tokio::spawn(ox_gateway::handle::InflightGc::open(
        broker.client(),
        path!("wire"),
        Record::parsed(Value::Bool(true)),
    ));
    accepted.notified().await;
    task.abort();
    let _ = task.await;
    // The original broker deadline elapses before an accepted reply exists.
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while live.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("eventual accepted reply must be GC'd");
}
