//! Command-line argument definitions, parsed by `clap`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use lanshare_core::transport::{TransportPreference, DEFAULT_PORT};

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
    /// Send files or folders to a device running `lanshare receive`.
    Send(SendArgs),
    /// List devices on the network that are running `lanshare receive`.
    Devices,
    /// Measure throughput to a device running `lanshare receive`, with generated data and no disk access.
    Bench(BenchArgs),
    /// Pair with a device running `lanshare receive`, so the two trust each other. It shows a PIN to type here.
    Pair(PairArgs),
    /// List or remove paired devices.
    #[command(subcommand)]
    Trusted(TrustedCommand),
    /// Show this device's name, id and certificate fingerprint.
    Whoami,
}

/// Arguments for `lanshare pair`.
#[derive(Debug, Args)]
pub struct PairArgs {
    /// Name of the device (see `lanshare devices`), or its IP address.
    pub target: String,
    /// Port the device listens on. Only used when the target is an IP address.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,
    /// Transport to use. `auto` tries QUIC and falls back to TCP.
    #[arg(long, value_enum, default_value_t = TransportArg::Auto)]
    pub transport: TransportArg,
}

/// `lanshare trusted` subcommands.
#[derive(Debug, Subcommand)]
pub enum TrustedCommand {
    /// List paired devices.
    List,
    /// Forget a paired device. Sending to or from it needs pairing again.
    Remove {
        /// The device's name, id, or fingerprint (as shown by `lanshare trusted list`).
        device: String,
    },
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
    /// Files and folders to send, in one transfer. Folders are sent with everything inside them.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Port the receiver listens on. Only used when the target is an IP address.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,
    /// Parallel data streams, 1 to 16. Defaults to `streams` in config.toml (4).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=16))]
    pub streams: Option<u8>,
    /// Bandwidth limit in MB/s; 0 means unlimited. Defaults to `bandwidth_limit_mbps` in config.toml.
    #[arg(long, value_name = "MB_PER_SECOND")]
    pub limit: Option<u32>,
    /// Transport to use. `auto` tries QUIC and falls back to TCP.
    #[arg(long, value_enum, default_value_t = TransportArg::Auto)]
    pub transport: TransportArg,
}

/// Arguments for `lanshare bench`.
#[derive(Debug, Args)]
pub struct BenchArgs {
    /// Name of the receiving device (see `lanshare devices`), or its IP address.
    pub target: String,
    /// How much data to send, e.g. `500MB`, `2GB` or `1GiB`.
    #[arg(long, default_value = "2GB", value_parser = parse_size)]
    pub size: u64,
    /// Parallel data streams, 1 to 16.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=16))]
    pub streams: u8,
    /// Transport to use. `auto` tries QUIC and falls back to TCP.
    #[arg(long, value_enum, default_value_t = TransportArg::Auto)]
    pub transport: TransportArg,
    /// Port the receiver listens on. Only used when the target is an IP address.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,
}

/// `--transport` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TransportArg {
    /// QUIC, falling back to TCP.
    Auto,
    /// TCP only.
    Tcp,
    /// QUIC only.
    Quic,
}

impl From<TransportArg> for TransportPreference {
    fn from(value: TransportArg) -> Self {
        match value {
            TransportArg::Auto => Self::Auto,
            TransportArg::Tcp => Self::Tcp,
            TransportArg::Quic => Self::Quic,
        }
    }
}

/// Parses sizes like `2GB` (decimal, 10^9), `1GiB` (binary, 2^30), `500MB` or a plain byte count.
fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().map_err(|_| format!("`{text}` does not start with a number"))?;
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        _ => return Err(format!("unknown unit in `{text}`; use B, KB, MB, GB, KiB, MiB or GiB")),
    };
    number.checked_mul(multiplier).filter(|size| *size > 0).ok_or_else(|| format!("`{text}` is not a usable size"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_with_decimal_and_binary_units() {
        assert_eq!(parse_size("2GB"), Ok(2_000_000_000));
        assert_eq!(parse_size("1GiB"), Ok(1 << 30));
        assert_eq!(parse_size("500 mb"), Ok(500_000_000));
        assert_eq!(parse_size("1234"), Ok(1234));
        assert!(parse_size("GB").is_err());
        assert!(parse_size("2TB").is_err());
        assert!(parse_size("0").is_err());
    }
}
