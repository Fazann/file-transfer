//! Everything the CLI shows to people: progress bars, prompts, status lines and English error text.

use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use indicatif::{DecimalBytes, HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use lanshare_core::{
    discovery::Peer,
    identity::{DeviceInfo, Os, TrustedDevice},
    pairing::{PairingPeer, LOCK_DURATION, PIN_ENTRY_TIMEOUT},
    service::{TransferId, TransferInfo},
    transfer::{BenchResult, IncomingOffer, Progress, ReceivedText},
    transport::TransportKind,
    Error, ErrorCode,
};
use tokio::{sync::watch, task::JoinHandle};

const PROGRESS_TEMPLATE: &str =
    "{wide_bar:.cyan/blue} {binary_bytes}/{binary_total_bytes} {binary_bytes_per_sec} ETA {eta} ({elapsed})";

/// Shows a progress bar driven by `receiver` until its sender is dropped.
///
/// The bar only appears after the first update, so nothing is drawn while a prompt is waiting for an answer. Each
/// time a connection resumes a partly sent file, a `Resuming: ...` line is printed above the bar.
pub fn spawn_progress_bar(mut receiver: watch::Receiver<Progress>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut bar: Option<ProgressBar> = None;
        let mut last_resumed = 0;
        // `changed()` returns an error once the transfer drops its sender, which ends the loop.
        while receiver.changed().await.is_ok() {
            let progress = *receiver.borrow_and_update();
            let bar = bar.get_or_insert_with(|| new_progress_bar(progress.total));
            bar.set_length(progress.total);
            bar.set_position(progress.transferred);
            if progress.resumed != last_resumed && progress.resumed > 0 {
                bar.println(resuming_line(progress.resumed, progress.total));
            }
            last_resumed = progress.resumed;
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

/// One progress bar per incoming transfer, for `lanshare receive`, which may receive several files at once.
pub struct TransferBars {
    bars: MultiProgress,
    active: HashMap<TransferId, (ProgressBar, TransferInfo, u64)>,
}

impl TransferBars {
    /// No bars yet.
    pub fn new() -> Self {
        Self { bars: MultiProgress::new(), active: HashMap::new() }
    }

    /// Adds a bar for a transfer that started.
    pub fn start(&mut self, info: TransferInfo) {
        let bar = self.bars.add(new_progress_bar(info.size));
        bar.set_message(info.file_name.clone());
        self.active.insert(info.id, (bar, info, 0));
    }

    /// Moves a bar; prints a `Resuming: ...` line when a transfer continues a partial file.
    pub fn update(&mut self, id: TransferId, progress: Progress) {
        let Some((bar, _, last_resumed)) = self.active.get_mut(&id) else { return };
        bar.set_length(progress.total);
        bar.set_position(progress.transferred);
        if progress.resumed != *last_resumed && progress.resumed > 0 {
            bar.println(resuming_line(progress.resumed, progress.total));
        }
        *last_resumed = progress.resumed;
    }

    /// Finishes a bar and says where the file was saved.
    pub fn complete(&mut self, id: TransferId, path: Option<&Path>) {
        let Some((bar, info, _)) = self.active.remove(&id) else { return };
        bar.finish_and_clear();
        if let Some(path) = path {
            println!("Saved {} ({}) from {}", path.display(), HumanBytes(info.size), printable(&info.peer_name));
        }
    }

    /// Drops a bar and says why the transfer failed.
    pub fn fail(&mut self, id: TransferId, error: &ErrorCode) {
        let Some((bar, info, _)) = self.active.remove(&id) else { return };
        bar.abandon();
        eprintln!("Receiving {} failed: {}", info.file_name, error_text(error));
        if matches!(error, ErrorCode::ConnectionLost | ErrorCode::Cancelled) {
            eprintln!("Anything already received was kept; sending the file again continues where it stopped.");
        }
    }
}

fn new_progress_bar(total: u64) -> ProgressBar {
    let style = ProgressStyle::with_template(PROGRESS_TEMPLATE)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("█▉▊▋▌▍▎▏ ");
    ProgressBar::new(total).with_style(style)
}

/// For example `Resuming: 1.20 GiB of 8.00 GiB already received`.
fn resuming_line(resumed: u64, total: u64) -> String {
    format!("Resuming: {} of {} already received", HumanBytes(resumed), HumanBytes(total))
}

/// The question asked before accepting an offer. Unpaired senders (only possible with `allow_untrusted`) are marked.
pub fn offer_question(offer: &IncomingOffer) -> String {
    let unpaired =
        if offer.trusted { String::new() } else { format!(", NOT PAIRED, fingerprint {}", offer.fingerprint) };
    let files = if offer.file_count == 1 { String::new() } else { format!("{} files, ", offer.file_count) };
    format!(
        "Accept {} ({files}{}) from {}{unpaired}? [y/N] ",
        printable(&offer.file_name),
        HumanBytes(offer.size),
        printable(&offer.device_name)
    )
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

/// Prints `question` and reads one line from stdin. `None` if stdin is closed or unreadable.
///
/// This blocks the thread, so call it inside `spawn_blocking`.
pub fn ask_line(question: &str) -> Option<String> {
    print!("{question}");
    io::stdout().flush().ok()?;
    let mut answer = String::new();
    match io::stdin().lock().read_line(&mut answer) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(answer.trim().to_owned()),
    }
}

/// Printed once the receiver is listening and advertising itself.
pub fn print_listening(info: &DeviceInfo, dir: &Path) {
    println!(
        "Receiving as \"{}\" (fingerprint {}) on port {}, saving to {}",
        info.name,
        info.fingerprint,
        info.port,
        dir.display()
    );
    println!("Press Ctrl+C to stop.");
}

/// Printed by the receiver when another device asks to pair.
pub fn print_pairing_pin(peer: &PairingPeer, pin: &str) {
    println!();
    println!("{} (fingerprint {}) wants to pair with this device.", printable(&peer.device_name), peer.fingerprint);
    println!("To allow it, type this PIN on that device within {} seconds:", PIN_ENTRY_TIMEOUT.as_secs());
    println!();
    println!("    {} {}", &pin[..3], &pin[3..]);
    println!();
}

/// The prompt for the PIN on the device that started pairing.
pub fn pin_question(receiver: &PairingPeer) -> String {
    format!(
        "Pairing with {} (fingerprint {}).\nEnter the PIN shown on that device: ",
        printable(&receiver.device_name),
        receiver.fingerprint
    )
}

/// Printed by `send` and `bench` before pairing with a receiver that is not paired yet.
pub fn print_pairing_needed() {
    println!("The two devices are not paired yet. Pairing first.");
}

/// Printed on both devices after pairing succeeded.
pub fn print_paired(device: &TrustedDevice) {
    println!("Paired with {} (fingerprint {}).", printable(&device.name), device.fingerprint);
}

/// Prints the result of `lanshare trusted list`.
pub fn print_trusted(devices: &[TrustedDevice]) {
    if devices.is_empty() {
        println!("No paired devices. Pair with `lanshare pair <device>`, or just send a file.");
        return;
    }
    for device in devices {
        let auto_accept = if device.auto_accept { "  auto-accept" } else { "" };
        println!(
            "{}  fingerprint {}  paired {}{auto_accept}",
            printable(&device.name),
            device.fingerprint,
            utc_date(device.paired_at)
        );
    }
}

/// Printed after `lanshare trusted remove`.
pub fn print_untrusted(device: &TrustedDevice) {
    println!(
        "Removed {} (fingerprint {}). It must pair again to exchange files.",
        printable(&device.name),
        device.fingerprint
    );
}

/// Error text when `trusted remove` finds no device.
pub fn no_trusted_device(query: &str) -> String {
    format!("No paired device matches \"{query}\". Run `lanshare trusted list` to see them.")
}

/// Error text when several paired devices share the name `trusted remove` was given.
pub fn ambiguous_trusted_device(query: &str) -> String {
    format!("Several paired devices are named \"{query}\". Use the fingerprint from `lanshare trusted list` instead.")
}

/// Prints `lanshare whoami`.
pub fn print_identity(info: &DeviceInfo) {
    println!("Name:        {}", info.name);
    println!("Device id:   {}", info.id);
    println!("Fingerprint: {}", info.fingerprint);
    println!("             {}", info.fingerprint.to_hex());
}

/// `YYYY-MM-DD` (UTC) for seconds since 1970-01-01.
fn utc_date(seconds: u64) -> String {
    // Days to civil date, from Howard Hinnant's `civil_from_days` (public domain), for days after 1970.
    let days = seconds / 86_400 + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Printed when the UDP port could not be opened, so only TCP works.
pub fn print_quic_unavailable(port: u16) {
    eprintln!("QUIC is unavailable (UDP port {port} could not be opened); accepting TCP connections only.");
}

/// Printed when the receiver stops.
pub fn print_stopped() {
    println!("Stopped.");
}

/// Printed while `lanshare devices` listens.
pub fn print_searching(duration: Duration) {
    println!("Looking for devices for {} seconds...", duration.as_secs());
}

/// Prints the result of `lanshare devices`. `is_paired` says which devices to mark as paired.
pub fn print_devices(peers: &[Peer], is_paired: impl Fn(&Peer) -> bool) {
    if peers.is_empty() {
        println!(
            "No devices found. Make sure LanShare is receiving on the other device and both are on the same network."
        );
        return;
    }
    for peer in peers {
        let paired = if is_paired(peer) { "  paired" } else { "" };
        println!("{}{paired}", device_line(peer));
    }
}

/// Printed before connecting to a device found by name.
pub fn print_connecting(peer: &Peer, paired: bool) {
    let paired = if paired { "" } else { " (not paired yet)" };
    println!("Connecting to {}{paired}", device_line(peer));
}

/// One line describing a device: name, OS, fingerprint and addresses.
fn device_line(peer: &Peer) -> String {
    let addresses: Vec<String> = peer.connect_order().iter().map(ToString::to_string).collect();
    format!("{}  ({})  {}  {}", printable(&peer.name), os_name(peer.os), peer.fingerprint, addresses.join(", "))
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

/// Printed once a connection is up, for example `Connected over QUIC`.
pub fn print_transport(kind: TransportKind) {
    println!("Connected over {}", transport_name(kind));
}

fn transport_name(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::Tcp => "TCP",
        TransportKind::Quic => "QUIC",
    }
}

/// Printed before a benchmark starts.
pub fn print_bench_start(size: u64, kind: TransportKind, streams: u8) {
    println!("Sending {} over {} with {streams} streams...", DecimalBytes(size), transport_name(kind));
}

/// The benchmark result, in MB/s (1 MB = 1,000,000 bytes).
pub fn print_bench_result(result: &BenchResult, kind: TransportKind) {
    println!(
        "{}: {} in {:.2} s, {:.1} MB/s ({} streams)",
        transport_name(kind),
        DecimalBytes(result.bytes),
        result.elapsed.as_secs_f64(),
        result.megabytes_per_second(),
        result.streams
    );
}

/// Printed when an offer is declined on the terminal.
pub fn print_declined(offer: &IncomingOffer) {
    println!("Declined {} from {}", offer.file_name, printable(&offer.device_name));
}

/// Printed when the answer came too late: the offer timed out or the sender left.
pub fn print_offer_expired(offer: &IncomingOffer) {
    println!("The offer of {} from {} is no longer waiting.", offer.file_name, printable(&offer.device_name));
}

/// Printed by the receiver when a pairing request failed.
pub fn print_pairing_failed(error: &ErrorCode) {
    eprintln!("Pairing failed: {}", error_text(error));
}

/// Printed by the receiver when it had to refuse an offer.
pub fn print_receive_refused(error: &ErrorCode) {
    eprintln!("Refused an incoming transfer: {}", error_text(error));
}

/// Printed after the files are sent.
pub fn print_sent(files: &[PathBuf]) {
    let names: Vec<String> = files.iter().map(|file| file.display().to_string()).collect();
    println!("Sent {}", names.join(", "));
}

/// Printed after a text is sent.
pub fn print_text_sent(device: &str) {
    println!("Text sent to {}", printable(device));
}

/// Prints a received text between two marker lines, so it is clear where it starts and ends.
pub fn print_text_received(text: &ReceivedText) {
    println!("--- Text from {} ---", printable(&text.device_name));
    println!("{}", printable_text(&text.text));
    println!("--- End of text ---");
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
        ErrorCode::HashMismatch => {
            "The received data did not match what was sent. The partial file was kept; try sending again.".to_owned()
        }
        ErrorCode::InsufficientSpace { needed, available } => format!(
            "Not enough free space on the receiving device: {} needed, {} available.",
            HumanBytes(*needed),
            HumanBytes(*available)
        ),
        ErrorCode::TransportUnavailable => {
            "Could not connect over QUIC or TCP. Check that LanShare is receiving on the other device and that a \
             firewall is not blocking port 52000."
                .to_owned()
        }
        ErrorCode::UntrustedDevice => {
            "The devices are not paired. Run `lanshare pair <device>` to pair them.".to_owned()
        }
        ErrorCode::FingerprintMismatch => {
            "The other device's certificate is not the one expected. It may be a different device using the same \
             name, or the device was reinstalled. If you trust it, remove it with `lanshare trusted remove` and pair \
             again."
                .to_owned()
        }
        ErrorCode::PairingFailed => "Pairing failed. Check the PIN and try again.".to_owned(),
        ErrorCode::PairingLocked { retry_after_secs } => format!(
            "Too many wrong PINs. Pairing is blocked for {} minutes; try again in {retry_after_secs} seconds.",
            LOCK_DURATION.as_secs() / 60
        ),
        ErrorCode::Busy => "The other device is busy with other connections. Try again in a moment.".to_owned(),
        ErrorCode::TextTooLong { max_bytes } => {
            format!(
                "The text is too long: at most {} can be sent as text. Send it as a file instead.",
                HumanBytes(*max_bytes)
            )
        }
        ErrorCode::PermissionDenied { folder } => {
            format!(
                "Cannot save to {folder} on the receiving device. Choose another download folder there, with `--dir` \
                 or in config.toml."
            )
        }
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

/// Like [`printable`], but keeps line breaks and tabs, for texts of several lines.
fn printable_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_removed() {
        assert_eq!(printable("evil\u{1b}[2Jname\n"), "evil[2Jname");
        assert_eq!(printable_text("line 1\r\n\tline\u{1b}[2J 2"), "line 1\n\tline[2J 2");
    }

    #[test]
    fn resuming_line_shows_both_sizes() {
        assert_eq!(resuming_line(512, 2048), "Resuming: 512 B of 2.00 KiB already received");
    }

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_793_145_599), "2026-10-27");
    }

    #[test]
    fn core_errors_get_english_text() {
        let error = user_facing_error(anyhow::Error::new(Error::Declined));
        assert_eq!(error.to_string(), "The receiver declined the file.");
    }
}
