//! `lanshare`: command-line interface for sending and receiving files on the local network.

mod args;
mod commands;
mod output;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::args::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr and default to warnings only, so they do not fight with the progress bar.
    // Set `RUST_LOG=debug` (or `RUST_LOG=lanshare_core=info`) for more detail.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    commands::run(cli.command).await.map_err(output::user_facing_error)
}
