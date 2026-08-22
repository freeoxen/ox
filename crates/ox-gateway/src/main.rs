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

    // Traffic log — opt-in via OX_GATEWAY_TRAFFIC_LOG ("1"/"true" for the
    // default ~/.ox/traffic.jsonl, or an explicit file path). Captures full
    // prompts and completions: the JSONL stream plus daily conversation
    // threads in ox ledger format under ~/.ox/threads.
    let traffic_enabled = match std::env::var("OX_GATEWAY_TRAFFIC_LOG") {
        Ok(v) if !v.is_empty() && v != "0" && v.to_lowercase() != "false" => Some(v),
        _ => None,
    };
    if let Some(setting) = &traffic_enabled {
        let jsonl_path = if setting == "1" || setting.to_lowercase() == "true" {
            ox_dir.join("traffic.jsonl")
        } else {
            std::path::PathBuf::from(setting)
        };
        let backing = ox_store_util::JsonlFileBacking::new(&jsonl_path)
            .context("opening traffic.jsonl backing")?;
        // The backing only creates the file on first append; create it now
        // so the 0600 clamp applies before any content lands. Traffic
        // records carry full prompt/completion text — owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&jsonl_path)
                .with_context(|| format!("creating {}", jsonl_path.display()))?;
            std::fs::set_permissions(&jsonl_path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("restricting {}", jsonl_path.display()))?;
        }
        let store = ox_gateway::traffic::TrafficLogStore::new(
            Box::new(backing),
            Some(ox_dir.join("threads")),
        );
        broker.mount(oxpath!("gateway", "traffic"), store).await;
        tracing::info!(path = %jsonl_path.display(), "traffic logging enabled");
    }


    // gateway/completions/ — CompletionBrokerStore with the production
    // SSE executor. Uses mount_async (the store is AsyncReader/AsyncWriter).
    let executor = Arc::new(
        ox_gate::transport::ReqwestSseExecutor::with_default_timeout()
            .map_err(anyhow::Error::msg)
            .context("constructing ReqwestSseExecutor")?,
    );
    let usage_client = broker.client().scoped("gateway/usage");
    // upstream/ — the SSE executor behind its own mount. The broker's
    // dispatch drains events from these paths instead of owning a socket;
    // the phase-3 broker Block runs against exactly this surface.
    let upstream_store =
        ox_gate::UpstreamStore::new(executor, tokio::runtime::Handle::current());
    broker.mount_async(oxpath!("upstream"), upstream_store).await;
    let mut completions = ox_gate::CompletionBrokerStore::new(
        broker.client(),
        broker.client().scoped("upstream"),
        usage_client,
        tokio::runtime::Handle::current(),
    );
    if traffic_enabled.is_some() {
        completions =
            completions.with_traffic_writer(broker.client().scoped("gateway/traffic"));
    }
    // OX_GATEWAY_WASM_BROKER=1: run each request's dispatch inside the
    // broker Block instead of the native task. Same substrate surface
    // either way; the parity suite holds the two together.
    let wasm_broker = matches!(
        std::env::var("OX_GATEWAY_WASM_BROKER").as_deref(),
        Ok(v) if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
    );
    if wasm_broker {
        let client = broker.client();
        let runtime = tokio::runtime::Handle::current();
        let traffic = traffic_enabled.is_some();
        completions = completions.with_block_runner(Arc::new(move |id| {
            if let Err(e) = ox_gateway::broker_block::run_broker(
                format!("gateway/completions/outstanding/{id}"),
                traffic,
                client.clone(),
                runtime.clone(),
            ) {
                tracing::error!(error = %e, id, "broker block run failed");
            }
        }));
        tracing::info!("broker block enabled (wasm dispatch)");
    }
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

    // OX_GATEWAY_WASM_WIRE=1: the dialect routes become the dumb http-in
    // edge and the wire Block owns each HTTP exchange end-to-end.
    let wasm_wire = matches!(
        std::env::var("OX_GATEWAY_WASM_WIRE").as_deref(),
        Ok(v) if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
    );
    let mut app = if wasm_wire {
        let runner_client = broker.client();
        let runtime = tokio::runtime::Handle::current();
        let wire_client = broker.client();
        let wire = ox_gateway::wire_store::WireStore::new(
            tokio::runtime::Handle::current(),
            Arc::new(move |id| {
                // The dialect rides in the inbound record; the runner reads
                // it back so the Block gets it in its config.
                let path = format!("wire/outstanding/{id}");
                let dialect = runtime
                    .block_on(async {
                        runner_client
                            .read(&structfs_core_store::Path::parse(&format!("{path}/inbound")).unwrap())
                            .await
                            .ok()
                            .flatten()
                            .and_then(|r| r.as_value().cloned())
                            .map(structfs_serde_store::value_to_json)
                    })
                    .and_then(|j| j["dialect"].as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "anthropic".into());
                if let Err(e) = ox_gateway::broker_block::run_wire(
                    path,
                    dialect,
                    runner_client.clone(),
                    runtime.clone(),
                ) {
                    tracing::error!(error = %e, id, "wire block run failed");
                }
            }),
        );
        broker.mount_async(oxpath!("wire"), wire).await;
        tracing::info!("wire block enabled (wasm http-in edge)");
        ox_gateway::routes::build_router_wire(broker.client())
    } else {
        ox_gateway::routes::build_router(broker.client())
    };
    if traffic_enabled.is_some() {
        app = app.layer(axum::middleware::from_fn_with_state(
            broker.client(),
            ox_gateway::traffic::http_log_middleware,
        ));
    }
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
