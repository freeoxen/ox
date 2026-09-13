//! Async Featherweight execution with independently supervised provider handles.
use crate::{assembly::WiringTable, codec_block};
use featherweight_runtime::{AssemblyDef, Runtime, async_host_store};
use ox_broker::ClientHandle;
use ox_gate::completion_broker::CancelHandle;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use structfs_core_store::{DetachedFuture, DetachedReader, DetachedWriter, Path, Record, Value};
use structfs_service::{OwnedResource, OwnerHandle};

type Handles = Arc<tokio::sync::Mutex<HashMap<Path, OwnedResource<Path>>>>;
struct BrokerImport {
    allocates: bool,
    base: Path,
    client: ClientHandle,
    owner: OwnerHandle,
    handles: Handles,
}
impl DetachedReader for BrokerImport {
    fn read_detached(&mut self, from: &Path) -> DetachedFuture<Option<Record>> {
        let target = self.base.join(from);
        let client = self.client.clone();
        Box::pin(async move {
            if target.to_string() == "sys/time/now_unix_ms" {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                return Ok(Some(Record::parsed(Value::Integer(now))));
            }
            client.read(&target).await
        })
    }
}
impl DetachedWriter for BrokerImport {
    fn write_detached(&mut self, to: &Path, data: Record) -> DetachedFuture<Path> {
        let base = self.base.clone();
        let target = base.join(to);
        let client = self.client.clone();
        let owner = self.owner.clone();
        let handles = self.handles.clone();
        // These are the gateway's allocating interfaces. They retain Ox's
        // outstanding/{id} protocol, while service owns their lifetime.
        let opens = to.is_empty() && self.allocates;
        Box::pin(async move {
            if opens {
                let cleanup_client = client.clone();
                let cleanup_base = base.clone();
                let handle = owner
                    .open(0, move |_| async move {
                        let relative = client.write_owned(&target, data).await?;
                        let absolute = cleanup_base.join(&relative);
                        Ok((relative, move || async move {
                            cleanup_client
                                .write_owned(&absolute, Record::parsed(Value::Null))
                                .await
                                .map(|_| ())
                        }))
                    })
                    .await?;
                let relative = handle.with(Clone::clone)?;
                handles.lock().await.insert(base.join(&relative), handle);
                Ok(relative)
            } else {
                let gc = matches!(data.as_value(), Some(Value::Null));
                let result = client.write_owned(&target, data).await?;
                if gc && let Some(handle) = handles.lock().await.remove(&target) {
                    handle.release();
                }
                Ok(result.strip_prefix(&base).unwrap_or(result.clone()))
            }
        })
    }
}

async fn run(
    config: serde_json::Value,
    wiring: WiringTable,
    cancel: CancelHandle,
    client: ClientHandle,
) -> Result<(), String> {
    // The detached task owns runtime, guest and provider cleanup even if its
    // HTTP caller is dropped. Store GC independently requests cancellation.
    let (send, recv) = tokio::sync::oneshot::channel();
    codec_block::executor().spawn(async move {
        let mut send = Some(send);
        let result = async {
            if cancel.is_cancelled() {
                return Err("gateway run cancelled".into());
            }
            let (block, _session) = codec_block::session().await?;
            if cancel.is_cancelled() {
                return Err("gateway run cancelled".into());
            }
            let mut runtime = Runtime::new().with_timeout(Duration::from_secs(600));
            runtime.register_core_artifact("embedded:gateway", block);
            let supervisor = runtime.cleanup_supervisor();
            let owner = supervisor
                .owner(Default::default())
                .map_err(|e| e.to_string())?;
            let handles = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let mut imports = HashMap::new();
            let mut wires = Vec::new();
            let mut descriptions = BTreeMap::new();
            for (index, (prefix, base)) in wiring.entries.iter().enumerate() {
                let name = format!("import{index}");
                descriptions.insert(name.clone(), "Ox substrate".to_string());
                wires.push(format!("guest:/{prefix} -> ${name}"));
                let allocates = matches!(
                    prefix.to_string().as_str(),
                    "upstream" | "gateway/completions" | "wire" | "gateway/telemetry"
                );
                imports.insert(
                    name,
                    async_host_store(BrokerImport {
                        allocates,
                        base: base.clone(),
                        client: client.clone(),
                        owner: owner.handle(),
                        handles: handles.clone(),
                    }),
                );
            }
            let mut merged = match wiring.config {
                None => serde_json::Map::new(),
                Some(value) => structfs_serde_store::value_to_json(value)
                    .map_err(|e| e.to_string())?
                    .as_object()
                    .cloned()
                    .ok_or("gateway block config must be an object")?,
            };
            merged.extend(config.as_object().unwrap().clone());
            let definition = serde_json::json!({
                "assembly": "gateway-request",
                "blocks": {"guest": "embedded:gateway"},
                "public": "guest",
                "imports": descriptions,
                "wiring": wires,
                "config": {"guest": merged}
            });
            let def = AssemblyDef::from_str(&definition.to_string()).map_err(|e| e.to_string())?;
            let instance = runtime
                .instantiate(&def, imports, ".".as_ref())
                .map_err(|e| e.to_string())?;
            tokio::select! {
                _ = instance.wait_public_terminal() => {},
                _ = cancel.cancelled() => {}
            }
            let outcome = instance.public_cell().last_error().or_else(|| {
                (instance.public_cell().exit_code() != 0)
                    .then(|| "gateway guest exited unsuccessfully".into())
            });
            owner.cancel();
            let mut report = instance.shutdown(Duration::from_secs(5)).await;
            let mut providers = owner.close(Duration::from_secs(5)).await;
            // Keep ownership and capacity until shutdown joins provider work.
            // Report incomplete cleanup while retaining the task that joins it.
            if (!report.complete() || !providers.is_quiescent())
                && let Some(send) = send.take()
            {
                let _ = send.send(Err(format!(
                    "gateway cleanup incomplete: {report:?}; {providers:?}"
                )));
            }
            while !report.complete() || !providers.is_quiescent() {
                tracing::error!(?report, ?providers, "gateway cleanup still pending");
                tokio::time::sleep(Duration::from_secs(1)).await;
                report = instance.shutdown(Duration::from_secs(5)).await;
                providers = owner.close(Duration::from_secs(5)).await;
            }
            if cancel.is_cancelled() {
                return Err("gateway run cancelled".into());
            }
            outcome.map_or(Ok(()), Err)
        }
        .await;
        if let Some(send) = send {
            let _ = send.send(result);
        }
    });
    recv.await.map_err(|e| e.to_string())?
}
pub async fn run_broker(
    inflight_path: String,
    traffic: bool,
    wiring: WiringTable,
    cancel: CancelHandle,
    client: ClientHandle,
    _runtime: tokio::runtime::Handle,
) -> Result<(), String> {
    run(
        serde_json::json!({"mode":"broker", "inflight":inflight_path,"traffic":traffic}),
        wiring,
        cancel,
        client,
    )
    .await
}
pub async fn run_wire(
    wire_path: String,
    dialect: String,
    wiring: WiringTable,
    cancel: CancelHandle,
    client: ClientHandle,
    _runtime: tokio::runtime::Handle,
) -> Result<(), String> {
    run(
        serde_json::json!({"mode":"wire", "wire":wire_path,"dialect":dialect}),
        wiring,
        cancel,
        client,
    )
    .await
}
pub async fn run_stats(
    telemetry_path: String,
    wiring: WiringTable,
    cancel: CancelHandle,
    client: ClientHandle,
    _runtime: tokio::runtime::Handle,
) -> Result<(), String> {
    run(
        serde_json::json!({"mode":"stats", "telemetry":telemetry_path}),
        wiring,
        cancel,
        client,
    )
    .await
}

/// Read the dialect supplied by the HTTP edge without converting corrupt
/// records into an unrelated default protocol.
pub async fn wire_dialect(client: &ClientHandle, wire: &str) -> Result<String, String> {
    let path = Path::parse(&format!("{wire}/inbound")).map_err(|e| e.to_string())?;
    let record = client
        .read(&path)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("wire inbound record missing")?;
    let value = record
        .as_value()
        .cloned()
        .ok_or("wire inbound record is raw")?;
    let json = structfs_serde_store::value_to_json(value).map_err(|e| e.to_string())?;
    json.get("dialect")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "wire inbound dialect missing".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_broker::{BrokerStore, async_store::BoxFuture};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use structfs_core_store::{DetachedReader, DetachedWriter};
    use structfs_core_store::{Error, path};

    struct Handles {
        live: Arc<AtomicUsize>,
        accepted: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl DetachedReader for Handles {
        fn read_detached(&mut self, _: &Path) -> BoxFuture<Result<Option<Record>, Error>> {
            Box::pin(async { Ok(None) })
        }
    }
    impl DetachedWriter for Handles {
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
                Ok(path!("outstanding/7"))
            })
        }
    }
    #[tokio::test]
    async fn owner_cleans_aliased_open_after_cancel_before_reply() {
        let broker = BrokerStore::new(Duration::from_millis(5));
        let live = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        broker
            .mount_async(
                path!("custom/backend"),
                Handles {
                    live: live.clone(),
                    accepted: accepted.clone(),
                    release: release.clone(),
                },
            )
            .await;
        let supervisor = structfs_service::CleanupSupervisor::new(8).unwrap();
        let owner = supervisor.owner(Default::default()).unwrap();
        let mut import = BrokerImport {
            allocates: true,
            base: path!("custom/backend"),
            client: broker.client(),
            owner: owner.handle(),
            handles: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        };
        let write =
            tokio::spawn(import.write_detached(&path!(""), Record::parsed(Value::Bool(true))));
        accepted.notified().await;
        owner.cancel();
        assert!(write.await.unwrap().is_err());
        tokio::time::sleep(Duration::from_millis(20)).await;
        release.notify_one();
        let report = owner.close(Duration::from_secs(2)).await;
        assert!(report.is_quiescent(), "{report:?}");
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn already_cancelled_run_has_no_broker_effects() {
        let wiring = crate::assembly::Manifest::embedded()
            .unwrap()
            .wiring_for("broker", &crate::assembly::standard_bindings())
            .unwrap();
        let cancel = CancelHandle::new();
        cancel.cancel();
        let broker = BrokerStore::new(Duration::from_millis(5));
        let result = run_broker(
            "gateway/completions/outstanding/0".into(),
            false,
            wiring,
            cancel,
            broker.client(),
            tokio::runtime::Handle::current(),
        )
        .await;
        assert!(result.unwrap_err().contains("cancelled"));
    }

    #[tokio::test]
    async fn wire_dialect_does_not_default_missing_inbound() {
        let broker = BrokerStore::new(Duration::from_millis(5));
        assert!(
            wire_dialect(&broker.client(), "wire/outstanding/0")
                .await
                .is_err()
        );
    }
}
