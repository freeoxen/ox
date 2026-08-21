mod action_executor;
mod agents;
mod app;
mod bindings;
mod broker_setup;
mod clash_sandbox;
mod commit_drain;
use ox_config::config;
mod dialogs;
mod editor;
#[cfg(test)]
mod editor_snapshots;
mod event_loop;
mod focus;
mod history_state;
mod history_view;
mod horns_loop;
mod inbox_shell;
mod inbox_view;
use ox_config::json_backing;
mod key_chord_canonical;
mod key_encode;
mod key_handlers;
mod key_migration;
mod parse;
mod policy;
mod policy_check;
#[allow(dead_code)]
mod session;
#[allow(dead_code)]
mod settings;
mod shell;
mod shell_copy;
mod simple_input;
mod tab_bar;
#[allow(dead_code)]
mod test_support;
mod text_input_view;
pub(crate) mod thread_registry;
mod thread_shell;
mod thread_view;
use ox_config::toml_backing;
mod tui;
mod types;
pub(crate) mod view_state;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Top-level application state. Each state owns its event loop; the
/// `main` function transitions between them by handing the terminal
/// from one to the next. See `event_loop::LegacyExit` and
/// `horns_loop::HornsExit` for the inter-state signals.
enum AppState {
    Legacy,
    Horns,
    Quit,
}

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

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let force_wizard = matches!(cli.command, Some(Commands::Init));
    // Setup wizard fires only when the user hasn't yet configured a usable
    // account. "Usable" includes unauthenticated providers (LM Studio,
    // Ollama) — they have no key file and never will, so a key-presence
    // check would re-trigger setup every launch.
    let needs_setup = force_wizard || !config::has_any_usable_account(&resolved);

    tracing::info!(
        force_wizard,
        needs_setup,
        accounts = resolved.gate.accounts.len(),
        default_account = %resolved.gate.defaults.account,
        model = %resolved.gate.defaults.model,
        "config resolved"
    );

    // Validate that the default account actually exists. Missing-key is no
    // longer a hard error — unauthenticated providers (LM Studio, Ollama) are
    // valid, and authenticated providers now surface a precise 401 message at
    // request time that names the URL, account, provider, and dialect.
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
    }

    // Keys never enter the flat config map any more — they're written
    // through the broker into `secret/keys/{name}: ApiKey` either by the
    // settings UI or by the one-shot startup migration of legacy `*.key`
    // files (see `migrate_legacy_keys` below).
    let flat_config = resolved.to_flat_map();

    let theme = horns_ratatui::Theme::default();

    // Setup broker with stores mounted
    let broker_inbox = ox_inbox::InboxStore::open(&inbox_root)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    let broker_bindings = bindings::default_bindings();
    let broker_handle = broker_setup::setup(
        broker_inbox,
        broker_bindings,
        inbox_root.clone(),
        flat_config,
    )
    .await;
    let client = broker_handle.client();

    // Wire the day-one settings subscriptions onto the broker before the
    // event loop starts its first frame. Each handler watches a typed
    // path under `config/gate/accounts/…` (or `config/save`) and reacts
    // to user-driven writes (test_now, refresh_now, delete_now, save)
    // — registering them here means the moment the user triggers an
    // action in the settings screen, the corresponding handler fires
    // automatically. Subscriptions are registered exactly once, on the
    // live `BrokerStore`, before any UI write can land.
    {
        use std::sync::Arc;
        let transport: Arc<dyn ox_gate::transport::Transport> =
            Arc::new(ox_gate::transport::HttpTransport);
        ox_gate::subscriptions::register_all(&broker_handle.broker, transport);
    }

    // Install the settings screen's framework-side subscriptions
    // (KeyDispatch / Render / ThemeChange + metadata writes). The
    // ratatui backend that actually owns the terminal is installed
    // by `run_horns_settings_loop` when the state machine transitions
    // into the Horns state — that way the terminal stays owned by
    // whichever state is currently driving the screen, with no
    // shared `Arc<Mutex<...>>` at the top level.
    let _settings_handle = match settings::install(&broker_handle.broker).await {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "settings::install failed");
            return Err(format!("settings::install failed: {e}").into());
        }
    };

    // Create App with broker. `Handle::current()` captures this
    // runtime so AgentPool workers (which run on their own OS threads
    // via thread::spawn) can bridge back to the broker via block_on.
    let mut app = app::App::new(
        workspace,
        inbox_root.clone(),
        cli.no_policy,
        broker_handle.broker.clone(),
        tokio::runtime::Handle::current(),
    )
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // Terminal is held in `Option<DefaultTerminal>` so the state
    // machine below can `.take()` it on entry to a state and put it
    // back when that state returns. Each state owns the terminal
    // exclusively for its event loop.
    let mut terminal: Option<ratatui::DefaultTerminal> = Some(ratatui::init());
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableFocusChange,
    )
    .ok();

    // One-shot migration of legacy `keys/*.key` files (and matching env
    // vars) into the broker's secrets namespace at `secret/keys/{name}:
    // ApiKey`. Idempotent: skips entirely when `secret/keys/*` is already
    // populated (post-migration runs see the JSON file load up front via
    // `ConfigStore::with_backing`). On a fresh install with no files and
    // no env vars set, this is a no-op.
    if let Err(e) = migrate_legacy_keys(&client, &keys_dir).await {
        tracing::warn!(error = %e, "legacy key migration encountered an error");
    }

    // Bootstrap the settings index entries and (when this is a fresh
    // install) seed the cursor at the new-account overlay. Both are
    // idempotent: re-running them on each launch is fine and keeps the
    // index in sync with day-one entries.
    if let Err(e) = settings::bootstrap::populate_index_entries(&client).await {
        tracing::warn!(error = %e, "failed to populate settings index entries");
    }
    match settings::bootstrap::maybe_first_run_cursor(&client).await {
        Ok(true) => tracing::info!("first-run: settings cursor seeded at _new overlay"),
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "first-run cursor seeding failed"),
    }

    // One-line log if the user's on-disk config still carries legacy
    // schema sections the new code no longer reads. Prevents support
    // questions of the form "where did my config go?".
    settings::bootstrap::log_legacy_settings_if_present(&inbox_root);

    // Top-level state machine. Each state owns the terminal for the
    // duration of its event loop and hands it back on exit. The legacy
    // event loop returns when the user navigates to a horns-owned
    // screen (settings) or quits; the horns event loop returns when
    // the user exits settings. Re-entry alternates between them
    // until quit.
    let result: std::io::Result<()> = {
        let mut state = AppState::Legacy;
        let mut needs_setup_arg = needs_setup;
        loop {
            match state {
                AppState::Legacy => {
                    // The legacy loop takes `&mut DefaultTerminal`,
                    // not by value, because the legacy frame logic
                    // calls `terminal.draw` repeatedly. Take the
                    // terminal out, hand a mut ref, then put it back.
                    let mut t = terminal.take().expect("terminal owned by main");
                    let outcome =
                        event_loop::run_async(&mut app, &client, &theme, &mut t, needs_setup_arg)
                            .await;
                    terminal = Some(t);
                    match outcome {
                        Ok(event_loop::LegacyExit::ToHorns) => {
                            needs_setup_arg = false;
                            state = AppState::Horns;
                        }
                        Ok(event_loop::LegacyExit::Quit) => state = AppState::Quit,
                        Err(e) => break Err(e),
                    }
                }
                AppState::Horns => {
                    // Hand the terminal in by value; the horns loop
                    // returns it when its session ends.
                    let t = terminal.take().expect("terminal owned by main");
                    match horns_loop::run_horns_settings_loop(&broker_handle.broker, &client, t)
                        .await
                    {
                        Ok((horns_loop::HornsExit::ToLegacy, t)) => {
                            terminal = Some(t);
                            state = AppState::Legacy;
                        }
                        Err(e) => break Err(e),
                    }
                }
                AppState::Quit => break Ok(()),
            }
        }
    };

    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableFocusChange,
    )
    .ok();
    ratatui::restore();

    // Persist runtime config changes to ~/.ox/config.toml
    client
        .write(
            &structfs_core_store::path!("config/save"),
            structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
        )
        .await
        .ok();

    tracing::info!("ox shutting down");
    result?;
    Ok(())
}

use crate::key_migration::migrate_legacy_keys;

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
