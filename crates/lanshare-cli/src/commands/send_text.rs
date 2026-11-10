//! `lanshare send-text <device-or-ip> [text]`: sends a short text to a paired receiver. The text comes from the
//! command line, or from standard input, so `pbpaste | lanshare send-text "Office PC"` sends the clipboard.

use std::io::Read;

use anyhow::bail;
use lanshare_core::{
    transfer::{check_text, send_text},
    transport::PeerCheck,
    Error,
};

use super::{pair, send::Destination, Context};
use crate::{args::SendTextArgs, output};

/// Reads the text, finds the receiver and sends it, pairing first if needed and possible.
pub async fn run(args: SendTextArgs, context: &Context) -> anyhow::Result<()> {
    let from_stdin = args.text.as_deref().is_none_or(|text| text == "-");
    let text = match args.text {
        Some(text) if !from_stdin => text,
        // Reading stdin blocks the thread, so it runs on tokio's blocking pool.
        _ => tokio::task::spawn_blocking(read_stdin).await??,
    };
    if text.is_empty() {
        bail!("There is no text to send.");
    }
    // Checked before looking for the device, which takes a few seconds.
    check_text(&text)?;

    let destination = Destination::resolve(&args.target, args.port, args.transport.into(), context).await?;
    match send_once(&destination, context, &text).await {
        // Pairing asks for the PIN on standard input, which already held the text.
        Err(Error::UntrustedDevice) if !from_stdin => {
            output::print_pairing_needed();
            pair::pair_with(&destination, context).await?;
            send_once(&destination, context, &text).await?;
        }
        result => result?,
    }
    output::print_text_sent(&args.target);
    Ok(())
}

async fn send_once(destination: &Destination, context: &Context, text: &str) -> lanshare_core::Result<()> {
    let connection = destination.connect(context, PeerCheck::Trusted(context.trust.clone())).await?;
    send_text(&connection, &context.device_info(), text).await
}

/// All of standard input, without one final line break (`echo` adds one).
fn read_stdin() -> anyhow::Result<String> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(without_final_line_break(text))
}

fn without_final_line_break(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_final_line_break_is_removed() {
        assert_eq!(without_final_line_break("hi\n".into()), "hi");
        assert_eq!(without_final_line_break("hi\r\n".into()), "hi");
        assert_eq!(without_final_line_break("a\n\n".into()), "a\n");
        assert_eq!(without_final_line_break("a\nb".into()), "a\nb");
    }
}
