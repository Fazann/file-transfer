//! Device id, operating system and the [`DeviceInfo`] a device announces about itself.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable, random id of a device. Generated once and stored in `config.toml`.
///
/// This is a "newtype": a struct with one field that gives `Uuid` its own type, so a `DeviceId` cannot be mixed up
/// with any other id. `#[serde(transparent)]` serializes it exactly like the inner `Uuid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(Uuid);

impl DeviceId {
    /// Creates a new random (v4) id.
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses the hyphenated text form, e.g. `"67e55044-10b1-426f-9247-bb680e5fe0c8"`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Uuid::parse_str(text).ok().map(Self)
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(formatter)
    }
}

/// Operating system of a device.
///
/// The variant order is part of the UDP announcement wire format: add new variants at the end only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Os {
    /// Microsoft Windows.
    Windows,
    /// Apple macOS.
    MacOs,
    /// Linux and other Unix-like systems.
    Linux,
}

impl Os {
    /// The operating system this program was compiled for.
    #[must_use]
    pub fn current() -> Self {
        // `cfg!` is evaluated at compile time, like Go build tags.
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }

    /// Short text form used in mDNS TXT records: `windows`, `macos` or `linux`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOs => "macos",
            Self::Linux => "linux",
        }
    }

    /// Parses the text form from [`Os::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "windows" => Some(Self::Windows),
            "macos" => Some(Self::MacOs),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }
}

/// Everything a device announces about itself on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable device id.
    pub id: DeviceId,
    /// Human-readable device name.
    pub name: String,
    /// Operating system.
    pub os: Os,
    /// TCP port the device receives transfers on.
    pub port: u16,
    /// Wire protocol version the device speaks.
    pub protocol_version: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_round_trips_through_text() {
        let id = DeviceId::new_random();
        assert_eq!(DeviceId::parse(&id.to_string()), Some(id));
        assert_eq!(DeviceId::parse("not-a-uuid"), None);
    }

    #[test]
    fn random_ids_differ() {
        assert_ne!(DeviceId::new_random(), DeviceId::new_random());
    }

    #[test]
    fn os_round_trips_through_text() {
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            assert_eq!(Os::parse(os.as_str()), Some(os));
        }
        assert_eq!(Os::parse("plan9"), None);
    }
}
