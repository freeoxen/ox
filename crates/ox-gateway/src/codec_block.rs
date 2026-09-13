//! Prepared Featherweight core guest and codec job execution.
use featherweight_runtime::{CoreWasmBlock, CoreWasmEngine};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
    time::Duration,
};
use structfs_core_store::{MemoryStore, Reader, Shared, Value, path};

static MODULE_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/codec_block.wasm"));
static EXECUTOR: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
type PreparedModule = (Arc<CoreWasmEngine>, Arc<CoreWasmBlock>);
static MODULE: tokio::sync::OnceCell<Result<PreparedModule, String>> =
    tokio::sync::OnceCell::const_new();

/// The prepared engine's epoch ticker must outlive every caller runtime.
/// This executor also supervises cleanup after a caller drops its run future.
pub(crate) fn executor() -> &'static tokio::runtime::Runtime {
    EXECUTOR.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("gateway-wasm")
            .enable_all()
            .build()
            .expect("gateway executor")
    })
}
pub(crate) async fn session() -> Result<
    (
        Arc<CoreWasmBlock>,
        Arc<featherweight_runtime::CoreWasmSession>,
    ),
    String,
> {
    let (engine, block) = MODULE
        .get_or_init(|| async {
            let engine = CoreWasmEngine::new(2).map_err(|e| e.to_string())?;
            let block = engine
                .prepare(MODULE_BYTES.to_vec())
                .await
                .map_err(|e| e.to_string())?;
            Ok((engine, Arc::new(block)))
        })
        .await
        .clone()?;
    let session = engine.reserve_session(1).map_err(|e| e.to_string())?;
    Ok((
        block
            .in_session(session.clone())
            .map_err(|e| e.to_string())?,
        session,
    ))
}

pub async fn run_job(job: serde_json::Value) -> Result<serde_json::Value, String> {
    executor()
        .spawn(async move {
            let (block, _session) = session().await?;
            let mut runtime = featherweight_runtime::Runtime::new();
            runtime.register_core_artifact("embedded:gateway", block);
            let def = featherweight_runtime::AssemblyDef::from_str(
                r#"
assembly: codec
blocks: {codec: "embedded:gateway"}
public: codec
imports: {codec: "codec jobs"}
wiring: ["codec:/codec -> $codec"]
config: {codec: {mode: codec}}
"#,
            )
            .map_err(|e| e.to_string())?;
            let mut store = Shared::new(MemoryStore::with_root(Value::Map(BTreeMap::from([(
                "job".into(),
                structfs_serde_store::json_to_value(job),
            )]))));
            let imports = [(
                "codec".into(),
                featherweight_runtime::host_store(store.clone()),
            )]
            .into();
            let instance = runtime
                .instantiate(&def, imports, ".".as_ref())
                .map_err(|e| e.to_string())?;
            instance.wait_public_terminal().await;
            let outcome = instance.public_cell().last_error().or_else(|| {
                (instance.public_cell().exit_code() != 0)
                    .then(|| "codec guest exited unsuccessfully".into())
            });
            let mut report = instance.shutdown(Duration::from_secs(5)).await;
            while !report.complete() {
                tracing::error!(?report, "codec cleanup incomplete");
                tokio::time::sleep(Duration::from_secs(1)).await;
                report = instance.shutdown(Duration::from_secs(5)).await;
            }
            if let Some(error) = outcome {
                return Err(error);
            }
            let result = store
                .read(&path!("result"))
                .map_err(|e| e.to_string())?
                .and_then(|record| record.as_value().cloned())
                .ok_or("codec block wrote no result")?;
            let json = structfs_serde_store::value_to_json(result).map_err(|e| e.to_string())?;
            if let Some(error) = json.get("error").and_then(|e| e.as_str()) {
                return Err(error.into());
            }
            Ok(json)
        })
        .await
        .map_err(|e| e.to_string())?
}
