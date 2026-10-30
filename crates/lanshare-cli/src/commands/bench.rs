//! `lanshare bench <device>`: measures throughput with generated data, without touching the disk on either side.

use lanshare_core::{transfer::run_bench, transport::PeerCheck, Error};

use super::{pair, send::Destination, Context};
use crate::{args::BenchArgs, output};

/// Connects to the receiver (pairing first if needed), runs one benchmark and prints the result.
pub async fn run(args: BenchArgs, context: &Context) -> anyhow::Result<()> {
    let destination = Destination::resolve(&args.target, args.port, args.transport.into(), context).await?;
    match bench_once(&args, context, &destination).await {
        Err(Error::UntrustedDevice) => {
            output::print_pairing_needed();
            pair::pair_with(&destination, context).await?;
            bench_once(&args, context, &destination).await?;
        }
        result => result?,
    }
    Ok(())
}

async fn bench_once(args: &BenchArgs, context: &Context, destination: &Destination) -> lanshare_core::Result<()> {
    let connection = destination.connect(context, PeerCheck::Trusted(context.trust.clone())).await?;
    output::print_bench_start(args.size, connection.kind(), args.streams);
    let result = run_bench(&connection, &context.device_info(), args.size, usize::from(args.streams)).await?;
    output::print_bench_result(&result, connection.kind());
    Ok(())
}
