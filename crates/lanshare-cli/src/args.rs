//! Command-line argument definitions, parsed by `clap`.

use std::{net::IpAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use lanshare_core::transport::DEFAULT_PORT;

/// Send large files between computers on the same network.
#[derive(Debug, Parser)]
#[command(name = "lanshare", version, about)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Wait for incoming files and save them.
    Receive(ReceiveArgs),
    /// Send a file to a computer running `lanshare receive`.
    Send(SendArgs),
}

/// Arguments for `lanshare receive`.
#[derive(Debug, Args)]
pub struct ReceiveArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,
    /// Folder to save received files in. Created if missing.
    #[arg(long, default_value = "./downloads")]
    pub dir: PathBuf,
    /// Accept every offer without asking.
    #[arg(long)]
    pub yes: bool,
}

/// Arguments for `lanshare send`.
#[derive(Debug, Args)]
pub struct SendArgs {
    /// IP address of the receiving computer.
    pub ip: IpAddr,
    /// File to send.
    pub file: PathBuf,
    /// Port the receiver listens on.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,
}
