//! Command-line argument definitions, parsed by `clap`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use lanshare_core::transport::DEFAULT_PORT;

/// Send large files between computers on the same network.
#[derive(Debug, Parser)]
#[command(name = "lanshare", version, about)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
    /// Disable mDNS and use only UDP broadcast. For testing networks that block mDNS.
    #[arg(long, global = true, hide = true)]
    pub no_mdns: bool,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Wait for incoming files and save them. Other devices can find this one by name while it runs.
    Receive(ReceiveArgs),
    /// Send a file to a device running `lanshare receive`.
    Send(SendArgs),
    /// List devices on the network that are running `lanshare receive`.
    Devices,
}

/// Arguments for `lanshare receive`.
#[derive(Debug, Args)]
pub struct ReceiveArgs {
    /// Port to listen on. Defaults to the port in config.toml (52000).
    #[arg(long)]
    pub port: Option<u16>,
    /// Folder to save received files in. Created if missing. Defaults to the folder in config.toml.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Accept every offer without asking.
    #[arg(long)]
    pub yes: bool,
}

/// Arguments for `lanshare send`.
#[derive(Debug, Args)]
pub struct SendArgs {
    /// Name of the receiving device (see `lanshare devices`), or its IP address.
    pub target: String,
    /// File to send.
    pub file: PathBuf,
    /// Port the receiver listens on. Only used when the target is an IP address.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,
}
