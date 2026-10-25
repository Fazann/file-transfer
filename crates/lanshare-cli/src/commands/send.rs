//! `lanshare send <ip> <file>`: sends one file to a receiver.

use std::{net::SocketAddr, process::Command};

use lanshare_core::{
    transfer::{progress_channel, send_file},
    transport,
};

use crate::{args::SendArgs, output};

/// Connects to the receiver and sends the file with a progress bar.
pub async fn run(args: SendArgs) -> anyhow::Result<()> {
    let device_name = tokio::task::spawn_blocking(local_device_name).await?;
    let mut stream = transport::connect(SocketAddr::new(args.ip, args.port)).await?;

    let (progress, watcher) = progress_channel();
    let bar = output::spawn_progress_bar(watcher);
    let result = send_file(&mut stream, &args.file, &device_name, &progress).await;
    // Dropping the sender tells the progress bar task to finish.
    drop(progress);
    bar.await?;

    result?;
    output::print_sent(&args.file);
    Ok(())
}

/// The computer's host name, shown to the receiver. Phase 2 replaces this with the saved device identity.
fn local_device_name() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}
