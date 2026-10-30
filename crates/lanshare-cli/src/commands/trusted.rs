//! `lanshare trusted list` and `lanshare trusted remove <device>`: the paired devices.

use anyhow::bail;
use lanshare_core::identity::{DeviceId, Fingerprint, TrustedDevice};

use super::Context;
use crate::{args::TrustedCommand, output};

/// Runs a `trusted` subcommand.
pub async fn run(command: TrustedCommand, context: &Context) -> anyhow::Result<()> {
    match command {
        TrustedCommand::List => {
            output::print_trusted(&context.trust.list());
            Ok(())
        }
        TrustedCommand::Remove { device } => {
            let found = find(&context.trust.list(), &device)?;
            context.trust.remove(found.id).await?;
            output::print_untrusted(&found);
            Ok(())
        }
    }
}

/// The one paired device that `query` names: by id, by fingerprint (full or short form), or by name ignoring case.
fn find(devices: &[TrustedDevice], query: &str) -> anyhow::Result<TrustedDevice> {
    let query = query.trim();
    let by_key = devices.iter().find(|device| {
        DeviceId::parse(query) == Some(device.id)
            || Fingerprint::parse_hex(query) == Some(device.fingerprint)
            || device.fingerprint.short().eq_ignore_ascii_case(query)
    });
    if let Some(device) = by_key {
        return Ok(device.clone());
    }
    let wanted = query.to_lowercase();
    let mut by_name: Vec<&TrustedDevice> =
        devices.iter().filter(|device| device.name.to_lowercase() == wanted).collect();
    match by_name.len() {
        0 => bail!(output::no_trusted_device(query)),
        1 => Ok(by_name.remove(0).clone()),
        _ => bail!(output::ambiguous_trusted_device(query)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, byte: u8) -> TrustedDevice {
        TrustedDevice::paired_now(DeviceId::new_random(), name.into(), Fingerprint::from_bytes([byte; 32]))
    }

    #[test]
    fn devices_are_found_by_name_id_or_fingerprint() {
        let devices = vec![device("Desk", 0xab), device("Phone", 0xcd)];
        assert_eq!(find(&devices, "desk").unwrap(), devices[0]);
        assert_eq!(find(&devices, &devices[1].id.to_string()).unwrap(), devices[1]);
        assert_eq!(find(&devices, "CDCD-CDCD-CDCD").unwrap(), devices[1]);
        assert_eq!(find(&devices, &devices[0].fingerprint.to_hex()).unwrap(), devices[0]);
        assert!(find(&devices, "Laptop").is_err());
    }

    #[test]
    fn a_shared_name_is_ambiguous() {
        let devices = vec![device("Desk", 1), device("Desk", 2)];
        assert!(find(&devices, "Desk").is_err());
        assert_eq!(find(&devices, &devices[1].fingerprint.short()).unwrap(), devices[1]);
    }
}
