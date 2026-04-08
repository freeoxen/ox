#[macro_use]
mod broker_cmd;
mod agents;
mod app;
mod bindings;
mod broker_setup;
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
    /// LLM provider (anthropic or openai)
    #[arg(long, default_value = "anthropic")]
    provider: String,

    /// Model identifier
    #[arg(long, short)]
    model: Option<String>,

    /// API key (or set ANTHROPIC_API_KEY / OPENAI_API_KEY env var)
    #[arg(long)]
    api_key: Option<String>,

    /// Workspace root directory
    #[arg(long, default_value = ".")]
    workspace: String,

    /// Max tokens per completion
    #[arg(long, default_value = "4096")]
    max_tokens: u32,

    /// Disable policy enforcement (allow all tool calls)
    #[arg(long)]
    no_policy: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let model = cli.model.unwrap_or_else(|| match cli.provider.as_str() {
        "openai" => "gpt-4o".to_string(),
        _ => "claude-sonnet-4-20250514".to_string(),
    });

    let api_key = cli.api_key.unwrap_or_else(|| match cli.provider.as_str() {
        "openai" => std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        _ => std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
    });

    if api_key.is_empty() {
        eprintln!("error: no API key provided");
        eprintln!("  pass --api-key or set ANTHROPIC_API_KEY / OPENAI_API_KEY");
        std::process::exit(1);
    }

    let workspace =
        std::fs::canonicalize(&cli.workspace).unwrap_or_else(|_| PathBuf::from(&cli.workspace));

    // Inbox root: ~/.ox
    let inbox_root = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ox")
    };

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
        cli.provider.clone(),
        model.clone(),
        cli.max_tokens,
        api_key.clone(),
    ));
    let client = broker_handle.client();

    // Create App with broker — pass rt handle so AgentPool workers can use it
    let mut app = app::App::new(
        cli.provider,
        model,
        cli.max_tokens,
        api_key,
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

    result?;
    Ok(())
}
