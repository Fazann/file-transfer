//! Everything the CLI shows to people: progress bars, prompts, status lines and English error text.

use std::{
    io::{self, BufRead, Write},
    path::Path,
    time::Duration,
};

use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
use lanshare_core::{
    discovery::Peer,
    identity::Os,
    transfer::{IncomingOffer, Progress, ReceivedFile},
    Error, ErrorCode,
};
use tokio::{sync::watch, task::JoinHandle};

const PROGRESS_TEMPLATE: &str =
    "{wide_bar:.cyan/blue} {binary_bytes}/{binary_total_bytes} {binary_bytes_per_sec} ETA {eta} ({elapsed})";

/// Shows a progress bar driven by `receiver` until its sender is dropped.
///
/// The bar only appears after the first update, so nothing is drawn while a prompt is waiting for an answer.
pub fn spawn_progress_bar(mut receiver: watch::Receiver<Progress>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut bar: Option<ProgressBar> = None;
        // `changed()` returns an error once the transfer drops its sender, which ends the loop.
        while receiver.changed().await.is_ok() {
            let progress = *receiver.borrow_and_update();
            let bar = bar.get_or_insert_with(|| new_progress_bar(progress.total));
            bar.set_length(progress.total);
            bar.set_position(progress.transferred);
        }
        if let Some(bar) = bar {
            if bar.position() == bar.length().unwrap_or_default() {
                bar.finish();
            } else {
                bar.abandon();
            }
        }
    })
}

fn new_progress_bar(total: u64) -> ProgressBar {
    let style = ProgressStyle::with_template(PROGRESS_TEMPLATE)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("█▉▊▋▌▍▎▏ ");
    ProgressBar::new(total).with_style(style)
}

/// The question asked before accepting an offer.
pub fn offer_question(offer: &IncomingOffer) -> String {
    format!("Accept {} ({}) from {}? [y/N] ", offer.file_name, HumanBytes(offer.size), printable(&offer.device_name))
}

/// Prints `question` and reads a yes/no answer from stdin. Anything but `y` or `yes` means no.
///
/// This blocks the thread, so call it inside `spawn_blocking`.
pub fn ask_yes_no(question: &str) -> bool {
    print!("{question}");
    if io::stdout().flush().is_err() {
        return false;
    }
    let mut answer = String::new();
    if io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Printed once the receiver is listening and advertising itself.
pub fn print_listening(device_name: &str, port: u16, dir: &Path) {
    println!("Receiving as \"{device_name}\" on port {port}, saving to {}", dir.display());
    println!("Press Ctrl+C to stop.");
}

/// Printed when the receiver stops.
pub fn print_stopped() {
    println!("Stopped.");
}

/// Printed while `lanshare devices` listens.
pub fn print_searching(duration: Duration) {
    println!("Looking for devices for {} seconds...", duration.as_secs());
}

/// Prints the result of `lanshare devices`.
pub fn print_devices(peers: &[Peer]) {
    if peers.is_empty() {
        println!(
            "No devices found. Make sure LanShare is receiving on the other device and both are on the same network."
        );
        return;
    }
    for peer in peers {
        println!("{}", device_line(peer));
    }
}

/// Printed before connecting to a device found by name.
pub fn print_connecting(peer: &Peer) {
    println!("Connecting to {}", device_line(peer));
}

/// One line describing a device: name, OS and addresses.
fn device_line(peer: &Peer) -> String {
    let addresses: Vec<String> = peer.connect_order().iter().map(ToString::to_string).collect();
    format!("{}  ({})  {}", printable(&peer.name), os_name(peer.os), addresses.join(", "))
}

fn os_name(os: Os) -> &'static str {
    match os {
        Os::Windows => "Windows",
        Os::MacOs => "macOS",
        Os::Linux => "Linux",
    }
}

/// Error text when no device has the given name.
pub fn no_device_named(name: &str) -> String {
    format!("No device named \"{name}\" was found. Run `lanshare devices` to see what is available.")
}

/// Error text when several devices share a name, listing each of them.
pub fn ambiguous_device_name(name: &str, peers: &[Peer]) -> String {
    let lines: Vec<String> = peers.iter().map(|peer| format!("  {}", device_line(peer))).collect();
    format!("Several devices are named \"{name}\". Send to one of their IP addresses instead:\n{}", lines.join("\n"))
}

/// Printed after a file is sent.
pub fn print_sent(file: &Path) {
    println!("Sent {}", file.display());
}

/// Printed after a file is received.
pub fn print_saved(received: &ReceivedFile) {
    println!(
        "Saved {} ({}) from {}",
        received.path.display(),
        HumanBytes(received.offer.size),
        printable(&received.offer.device_name)
    );
}

/// Printed after an offer is declined.
pub fn print_declined(offer: &IncomingOffer) {
    println!("Declined {} from {}", offer.file_name, printable(&offer.device_name));
}

/// Printed when one incoming transfer fails; the receiver keeps waiting for the next one.
pub fn print_receive_failed(error: &Error) {
    eprintln!("Receive failed: {}", describe_error(error));
}

/// English text for an error code.
pub fn error_text(code: &ErrorCode) -> String {
    match code {
        ErrorCode::Declined => "The receiver declined the file.".to_owned(),
        ErrorCode::VersionMismatch { ours, theirs } => format!(
            "The other device uses protocol version {theirs}, but this one uses version {ours}. \
             Update LanShare on both devices."
        ),
        ErrorCode::InvalidFileName => "The file name is not allowed.".to_owned(),
        ErrorCode::Io => "A file or network operation failed.".to_owned(),
        ErrorCode::ProtocolViolation => "The other device sent unexpected data.".to_owned(),
        ErrorCode::ConnectionLost => "The connection was lost.".to_owned(),
        ErrorCode::Cancelled => "The transfer was cancelled.".to_owned(),
        ErrorCode::PeerUnreachable => "Could not connect to the device on any of its addresses.".to_owned(),
    }
}

/// English text for an error, with the operating system's message added for I/O errors.
pub fn describe_error(error: &Error) -> String {
    let text = error_text(&error.code());
    match error {
        Error::Io(io_error) => format!("{text} ({io_error})"),
        _ => text,
    }
}

/// Replaces a `lanshare_core::Error` inside `error` with its English text, so `main` prints something readable.
pub fn user_facing_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<Error>() {
        Some(core_error) => anyhow::anyhow!(describe_error(core_error)),
        None => error,
    }
}

/// Removes control characters from peer-supplied text so it cannot mess with the terminal.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_removed() {
        assert_eq!(printable("evil\u{1b}[2Jname\n"), "evil[2Jname");
    }

    #[test]
    fn core_errors_get_english_text() {
        let error = user_facing_error(anyhow::Error::new(Error::Declined));
        assert_eq!(error.to_string(), "The receiver declined the file.");
    }
}
