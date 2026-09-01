use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ox_worker::{WorkerConfig, WorkerLimits, WorkerService};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        attempt_id: String,
        #[arg(long, default_value_t = 64)]
        command_capacity: usize,
        #[arg(long, default_value_t = 8)]
        max_active_turns: usize,
        #[arg(long, default_value_t = 16)]
        max_queued_inputs_per_thread: usize,
        #[arg(long, default_value_t = 256)]
        max_total_threads: usize,
        #[arg(long, default_value_t = 64)]
        max_parked_cursors: usize,
        #[arg(long, default_value_t = 256)]
        max_ledger_batch_entries: usize,
        #[arg(long, default_value_t = 1_048_576)]
        max_ledger_batch_bytes: usize,
        #[arg(long, default_value_t = 262_144)]
        max_ledger_line_bytes: usize,
    },
    StructfsStdio {
        #[arg(long)]
        socket: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Serve {
            root,
            socket,
            node_id,
            attempt_id,
            command_capacity,
            max_active_turns,
            max_queued_inputs_per_thread,
            max_total_threads,
            max_parked_cursors,
            max_ledger_batch_entries,
            max_ledger_batch_bytes,
            max_ledger_line_bytes,
        } => {
            let limits = WorkerLimits {
                max_active_turns,
                max_queued_inputs_per_thread,
                max_total_threads,
                max_parked_cursors,
                max_ledger_batch_entries,
                max_ledger_batch_bytes,
                max_ledger_line_bytes,
            };
            let service = WorkerService::start(WorkerConfig {
                inbox_root: root,
                socket_path: socket,
                node_id,
                attempt_id,
                command_capacity,
                limits,
                transport: ox_structfs_transport::ServerConfig::default(),
            })
            .await?;
            tokio::signal::ctrl_c().await?;
            service.shutdown().await?;
        }
        Command::StructfsStdio { socket } => {
            ox_structfs_transport::bridge_stdio_to_unix(socket).await?;
        }
    }
    Ok(())
}
