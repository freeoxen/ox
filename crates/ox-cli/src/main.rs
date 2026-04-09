#[macro_use]
mod broker_cmd;
mod agents;
mod app;
mod bindings;
mod broker_setup;
mod config;
mod dialogs;
mod event_loop;
mod inbox_view;
mod key_encode;
mod key_handlers;
mod parse;
mod policy;
#[allow(dead_code)]
mod session;
mod tab_bar;
mod theme;
pub(crate) mod thread_registry;
mod thread_view;
mod toml_backing;
mod tools;
mod transport;
mod tui;
mod types;
pub(crate) mod view_state;

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ox", about = "Agentic coding CLI")]
struct Cli {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let workspace =
        std::fs::canonicalize(&cli.workspace).unwrap_or_else(|_| PathBuf::from(&cli.workspace));

    // Inbox root: ~/.ox
    let inbox_root = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ox")
    };

    // Resolve config: defaults → ~/.ox/config.toml → OX_* env vars → CLI flags
    let overrides = config::CliOverrides {
        account: cli.account.clone(),
        model: cli.model.clone(),
        max_tokens: cli.max_tokens.map(|t| t as i64),
    };
    let resolved = config::resolve_config(&inbox_root, &overrides);

    // Verify the default account has a key (from config file or env vars)
    let default_account = &resolved.gate.defaults.account;
    let has_key = resolved
        .gate
        .accounts
        .get(default_account)
        .map(|a| !a.key.is_empty())
        .unwrap_or(false);
    if !has_key {
        eprintln!("error: no API key for account '{default_account}'");
        eprintln!(
            "  configure in ~/.ox/config.toml under [gate.accounts.{default_account}]"
        );
        eprintln!(
            "  or set OX_GATE__ACCOUNTS__{}_KEY",
            default_account.to_uppercase()
        );
        std::process::exit(1);
    }

    let flat_config = resolved.to_flat_map();

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
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture).ok();

    let result = rt.block_on(event_loop::run_async(
        &mut app,
        &client,
        &theme,
        &mut terminal,
    ));

    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture).ok();
    ratatui::restore();

    // Persist runtime config changes to ~/.ox/config.toml
    rt.block_on(client.write(
        &structfs_core_store::path!("config/save"),
        structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
    ))
    .ok();

    result?;
    Ok(())
}
