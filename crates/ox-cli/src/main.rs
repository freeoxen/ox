#[macro_use]
mod broker_cmd;
mod agents;
mod app;
mod bindings;
mod broker_setup;
mod clash_sandbox;
mod config;
mod dialogs;
mod editor;
mod event_loop;
mod inbox_view;
mod key_encode;
mod key_handlers;
mod parse;
mod policy;
mod policy_check;
#[allow(dead_code)]
mod session;
mod settings_state;
mod settings_view;
mod tab_bar;
mod text_input_view;
mod theme;
pub(crate) mod thread_registry;
mod thread_view;
mod toml_backing;
mod transport;
mod tui;
mod types;
pub(crate) mod view_state;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ox", about = "Agentic coding CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Named account from config (overrides gate.defaults.account)
    #[arg(long)]
    account: Option<String>,

    /// Model identifier
    #[arg(long, short)]
    model: Option<String>,

    /// Workspace root directory
    #[arg(long, default_value = ".")]
    workspace: String,

    /// Max tokens per completion
    #[arg(long)]
    max_tokens: Option<u32>,

    /// Disable policy enforcement (allow all tool calls)
    #[arg(long)]
    no_policy: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive setup wizard
    Init,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let workspace =
        std::fs::canonicalize(&cli.workspace).unwrap_or_else(|_| PathBuf::from(&cli.workspace));

    // Inbox root: ~/.ox
    let inbox_root = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ox")
    };

    // Set up tracing → per-run log file under ~/.ox/logs/
    let _guard = setup_tracing(&inbox_root);

    tracing::info!(
        workspace = %workspace.display(),
        inbox_root = %inbox_root.display(),
        "ox starting"
    );

    // Resolve config: defaults → ~/.ox/config.toml → OX_* env vars → CLI flags
    let overrides = config::CliOverrides {
        account: cli.account.clone(),
        model: cli.model.clone(),
        max_tokens: cli.max_tokens.map(|t| t as i64),
    };
    let resolved = config::resolve_config(&inbox_root, &overrides);

    let keys_dir = inbox_root.join("keys");
    let resolved_keys = config::resolve_keys(&keys_dir, &resolved);
    let force_wizard = matches!(cli.command, Some(Commands::Init));
    let needs_setup = force_wizard || !config::has_any_key(&keys_dir, &resolved);

    tracing::info!(
        force_wizard,
        needs_setup,
        accounts = resolved.gate.accounts.len(),
        keys = resolved_keys.len(),
        default_account = %resolved.gate.defaults.account,
        model = %resolved.gate.defaults.model,
        "config resolved"
    );

    // Validate config: catch mismatches that would silently cause 401s
    if !needs_setup {
        let default_acct = &resolved.gate.defaults.account;
        if !resolved.gate.accounts.contains_key(default_acct) {
            let available: Vec<&str> = resolved.gate.accounts.keys().map(|s| s.as_str()).collect();
            tracing::error!(
                default_account = %default_acct,
                available = ?available,
                "default account not found in configured accounts"
            );
            eprintln!(
                "error: default account '{}' not found in config.\n\
                 Available accounts: {}\n\
                 Run `ox init` to reconfigure, or edit ~/.ox/config.toml",
                default_acct,
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            );
            std::process::exit(1);
        }
        if !resolved_keys.contains_key(default_acct) {
            tracing::error!(
                default_account = %default_acct,
                "no API key found for default account"
            );
            eprintln!(
                "error: no API key for account '{}'.\n\
                 Run `ox init` to reconfigure, or add key to ~/.ox/keys/{}.key",
                default_acct, default_acct
            );
            std::process::exit(1);
        }
    }

    let flat_config = resolved.to_flat_map_with_keys(&resolved_keys);

    let theme = theme::Theme::default();

    // Create tokio runtime for broker
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Setup broker with stores mounted
    let broker_inbox = ox_inbox::InboxStore::open(&inbox_root)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    let broker_bindings = bindings::default_bindings();
    let broker_handle = rt.block_on(broker_setup::setup(
        broker_inbox,
        broker_bindings,
        inbox_root.clone(),
        flat_config,
    ));
    let client = broker_handle.client();

    // Create App with broker — pass rt handle so AgentPool workers can use it
    let mut app = app::App::new(
        workspace,
        inbox_root.clone(),
        cli.no_policy,
        broker_handle.broker.clone(),
        rt.handle().clone(),
    )
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
    )
    .ok();

    let result = rt.block_on(event_loop::run_async(
        &mut app,
        &client,
        &theme,
        &mut terminal,
        needs_setup,
    ));

    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
    )
    .ok();
    ratatui::restore();

    // Persist runtime config changes to ~/.ox/config.toml
    rt.block_on(client.write(
        &structfs_core_store::path!("config/save"),
        structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
    ))
    .ok();

    tracing::info!("ox shutting down");
    result?;
    Ok(())
}

/// Set up tracing with a per-run log file under `{inbox_root}/logs/`.
///
/// Returns a guard that must be held for the lifetime of the program to
/// ensure the non-blocking writer flushes on drop.
fn setup_tracing(inbox_root: &std::path::Path) -> tracing_appender::non_blocking::WorkerGuard {
    let logs_dir = inbox_root.join("logs");
    std::fs::create_dir_all(&logs_dir).ok();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let log_path = logs_dir.join(format!("ox-{now}.log"));
    let log_file = std::fs::File::create(&log_path).expect("failed to create log file");

    let (writer, guard) = tracing_appender::non_blocking(log_file);

    let filter = tracing_subscriber::EnvFilter::try_from_env("OX_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_env_filter(filter)
        .finish();

    if tracing::subscriber::set_global_default(subscriber).is_err() {
        eprintln!("warning: tracing subscriber already set, logs may be missing");
    }

    guard
}
