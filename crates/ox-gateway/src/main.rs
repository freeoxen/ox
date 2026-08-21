//! ox-gateway entry point. Assembles the shared broker (config + secret +
//! gate + gateway/usage + gateway/completions), then serves axum on
//! 127.0.0.1:11343 (configurable via OX_GATEWAY_BIND).
//!
//! Config resolution and the TOML/JSON file backings come from ox-config —
//! the same code ox-cli runs, so the two daemons cannot drift on how
//! ~/.ox/config.toml and keys.json are interpreted.

use anyhow::Context;
use ox_broker::{BrokerStore, SyncClientAdapter};
use ox_path::oxpath;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let ox_dir = ox_dir()?;
    let toml_path = ox_dir.join("config.toml");
    let keys_path = ox_dir.join("keys.json");
    let usage_path = ox_dir.join("usage.jsonl");

    // Generous client timeout: the completion drain's events/from read
    // legitimately parks for the whole inter-token gap, which for thinking
    // models can be minutes. A short timeout here turns normal upstream
    // latency into fatal error frames on healthy requests.
    let broker = BrokerStore::new(Duration::from_secs(600));

    // config/ — ConfigStore over the same TOML ox-cli reads.
    // The figment-resolved base flat map is loaded from the file; runtime
    // overrides accumulate in memory (and save back via a separate path).
    let ox_config = ox_config::resolve_config(&ox_dir, &ox_config::CliOverrides::default());
    let base = ox_config.to_flat_map();
    let config_backing = ox_config::TomlFileBacking::new(toml_path.clone());
    let config = ox_ui::ConfigStore::with_backing(base, Box::new(config_backing));
    broker.mount(oxpath!("config"), config).await;

    // secret/ — ConfigStore over keys.json (same store type, different file).
    let secret_backing = ox_config::JsonFileBacking::new(keys_path.clone());
    let secret = ox_ui::ConfigStore::with_backing(
        std::collections::BTreeMap::new(),
        Box::new(secret_backing),
    );
    broker.mount(oxpath!("secret"), secret).await;

    // gate/ — GateStore wired to config + secret handles. Same wiring
    // ox-cli uses; this is the cross-process shared substrate.
    let rt = tokio::runtime::Handle::current();
    let config_adapter = SyncClientAdapter::new(broker.client().scoped("config"), rt.clone());
    let secret_adapter = SyncClientAdapter::new(broker.client().scoped("secret"), rt.clone());
    let gate = ox_gate::GateStore::new()
        .with_config(Box::new(config_adapter))
        .with_secrets(Box::new(secret_adapter));
    broker.mount(oxpath!("gate"), gate).await;

    // gateway/usage/ — JsonlFileBacking over ~/.ox/usage.jsonl.
    let usage_backing = Box::new(
        ox_store_util::JsonlFileBacking::new(&usage_path)
            .context("opening usage.jsonl backing")?,
    );
    let usage = ox_gate::UsageStore::new(usage_backing);
    broker.mount(oxpath!("gateway", "usage"), usage).await;

    // gateway/completions/ — CompletionBrokerStore with the production
    // SSE executor. Uses mount_async (the store is AsyncReader/AsyncWriter).
    let executor = Arc::new(
        ox_gate::transport::ReqwestSseExecutor::with_default_timeout()
            .map_err(anyhow::Error::msg)
            .context("constructing ReqwestSseExecutor")?,
    );
    let usage_client = broker.client().scoped("gateway/usage");
    let completions = ox_gate::CompletionBrokerStore::new(
        broker.client(),
        executor,
        usage_client,
        tokio::runtime::Handle::current(),
    );
    broker.mount_async(oxpath!("gateway", "completions"), completions).await;

    // Same gate subscriptions ox-cli registers (catalog refresh, account
    // test/delete, config save), then kick a catalog refresh per account so
    // /v1/models serves real entries shortly after boot instead of an empty
    // list until something else triggers a refresh.
    {
        let transport: Arc<dyn ox_gate::transport::Transport> =
            Arc::new(ox_gate::transport::HttpTransport);
        ox_gate::subscriptions::register_all(&broker, transport);
        let client = broker.client();
        for name in ox_config.gate.accounts.keys() {
            let Ok(comp) = ox_kernel::PathComponent::try_new(name) else {
                continue;
            };
            let path = oxpath!("config", "gate", "accounts", comp, "refresh_now");
            if let Err(e) = client
                .write(&path, structfs_core_store::Record::parsed(structfs_core_store::Value::Null))
                .await
            {
                tracing::warn!(account = %name, error = %e, "catalog refresh trigger failed");
            }
        }
    }

    // axum
    let bind_addr =
        std::env::var("OX_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:11343".into());
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %listener.local_addr()?, "ox-gateway listening");

    let app = ox_gateway::routes::build_router(broker.client());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum::serve")?;
    tracing::info!("ox-gateway shut down");
    Ok(())
}

/// Resolve on SIGINT (ctrl-c) or SIGTERM so in-flight requests get to
/// drain instead of being severed mid-stream.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining");
}

fn ox_dir() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME is not set"))?;
    let dir = std::path::PathBuf::from(home).join(".ox");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}
