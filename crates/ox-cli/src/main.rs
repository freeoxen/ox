mod app;
mod theme;
mod tools;
mod transport;
mod tui;

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

    let mut app = app::App::new(cli.provider, model, cli.max_tokens, api_key, workspace);

    let theme = theme::Theme::default();

    let mut terminal = ratatui::init();
    let result = tui::run(&mut app, &theme, &mut terminal);
    ratatui::restore();

    result?;
    Ok(())
}
