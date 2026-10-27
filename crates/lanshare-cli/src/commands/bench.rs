//! `lanshare bench <device>`: measures throughput with generated data, without touching the disk on either side.

use lanshare_core::{config::Config, transfer::run_bench};

use super::send::Destination;
use crate::{args::BenchArgs, output};

/// Connects to the receiver, runs one benchmark and prints the result.
pub async fn run(args: BenchArgs, config: &Config, use_mdns: bool) -> anyhow::Result<()> {
    let destination = Destination::resolve(&args.target, args.port, args.transport.into(), config, use_mdns).await?;
    let connection = destination.connect().await?;
    output::print_bench_start(args.size, connection.kind(), args.streams);
    let result = run_bench(&connection, &config.device_info(), args.size, usize::from(args.streams)).await?;
    output::print_bench_result(&result, connection.kind());
    Ok(())
}
