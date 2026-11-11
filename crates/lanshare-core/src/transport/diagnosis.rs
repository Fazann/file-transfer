//! Why a connection could not be made, and what the user can do about it.
//!
//! [`connect`](super::connect) and [`connect_any`](super::connect_any) record every attempt they make in a
//! [`ConnectReport`], carried by [`Error::PeerUnreachable`](crate::Error::PeerUnreachable) and
//! [`Error::TransportUnavailable`](crate::Error::TransportUnavailable). [`ConnectReport::diagnose`] turns it into a
//! [`ConnectDiagnosis`]: the outcome per transport plus a [`ConnectHint`] for the user.
//!
//! None of this goes on the wire; the error code sent to other devices is unchanged.

use std::{fmt, io, net::SocketAddr};

use super::TransportKind;
use crate::{error::Error, identity::Os};

/// Why one connection attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectFailure {
    /// Nothing answered in time. Usually a firewall that drops packets, or a device that left.
    TimedOut,
    /// The device answered that nothing accepts connections on the port.
    Refused,
    /// The operating system would not send to the address. On macOS this is what a missing Local Network
    /// permission looks like; elsewhere usually a firewall that rejects packets.
    NoRoute,
    /// This device has no network to send on.
    NetworkDown,
    /// Anything else, such as a failed TLS handshake.
    Other,
}

impl ConnectFailure {
    /// Classifies the error of one attempt.
    pub(crate) fn of(error: &Error) -> Self {
        let Error::Io(error) = error else { return Self::Other };
        match error.kind() {
            io::ErrorKind::TimedOut => Self::TimedOut,
            io::ErrorKind::ConnectionRefused => Self::Refused,
            io::ErrorKind::HostUnreachable => Self::NoRoute,
            io::ErrorKind::NetworkUnreachable | io::ErrorKind::NetworkDown => Self::NetworkDown,
            _ => Self::Other,
        }
    }

    /// Fixed `camelCase` name, used as a translation key by the app, e.g. `"noRoute"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::TimedOut => "timedOut",
            Self::Refused => "refused",
            Self::NoRoute => "noRoute",
            Self::NetworkDown => "networkDown",
            Self::Other => "other",
        }
    }

    /// How much the failure says about the other device: an answer (refused) says most, a local problem least.
    fn rank(self) -> u8 {
        match self {
            Self::Refused => 0,
            Self::TimedOut => 1,
            Self::NoRoute => 2,
            Self::Other => 3,
            Self::NetworkDown => 4,
        }
    }
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TimedOut => "timed out",
            Self::Refused => "connection refused",
            Self::NoRoute => "no route to host",
            Self::NetworkDown => "network unreachable",
            Self::Other => "failed",
        })
    }
}

/// One failed attempt: an address over one transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectAttempt {
    /// Where.
    pub addr: SocketAddr,
    /// Over which transport.
    pub transport: TransportKind,
    /// Why it failed.
    pub failure: ConnectFailure,
}

/// Every failed attempt of one connect call, plus the other device's operating system when discovery knew it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectReport {
    /// The attempts, in the order they were made.
    pub attempts: Vec<ConnectAttempt>,
    /// The other device's operating system, for firewall hints.
    pub peer_os: Option<Os>,
}

/// What the user can do about a failed connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectHint {
    /// macOS refused to send to the local network: turn on `LanShare` under Privacy & Security → Local Network.
    MacosLocalNetwork,
    /// This device is not on a network.
    NoNetwork,
    /// The other device answered but nothing listens on the port: `LanShare` is probably closed there.
    NotRunning,
    /// A firewall on the other device, which runs Linux, probably blocks the port.
    FirewallLinux,
    /// A firewall on the other device, which runs Windows, probably blocks the port.
    FirewallWindows,
    /// A firewall on the other device, which runs macOS, probably blocks the port.
    FirewallMacos,
    /// A firewall on the other device, whose operating system is not known, probably blocks the port.
    Firewall,
}

impl ConnectHint {
    /// Fixed `camelCase` name, used as a translation key by the app, e.g. `"macosLocalNetwork"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::MacosLocalNetwork => "macosLocalNetwork",
            Self::NoNetwork => "noNetwork",
            Self::NotRunning => "notRunning",
            Self::FirewallLinux => "firewallLinux",
            Self::FirewallWindows => "firewallWindows",
            Self::FirewallMacos => "firewallMacos",
            Self::Firewall => "firewall",
        }
    }
}

/// A summary of a [`ConnectReport`] for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectDiagnosis {
    /// The most telling QUIC failure, or `None` if QUIC was not tried.
    pub quic: Option<ConnectFailure>,
    /// The most telling TCP failure, or `None` if TCP was not tried.
    pub tcp: Option<ConnectFailure>,
    /// The port that was tried.
    pub port: u16,
    /// What to do about it, if anything specific is known.
    pub hint: Option<ConnectHint>,
}

impl ConnectReport {
    /// Records a failed attempt.
    pub(crate) fn push(&mut self, addr: SocketAddr, transport: TransportKind, error: &Error) {
        self.attempts.push(ConnectAttempt { addr, transport, failure: ConnectFailure::of(error) });
    }

    /// The summary for the user, seen from this device. `None` if nothing was attempted.
    #[must_use]
    pub fn diagnose(&self) -> Option<ConnectDiagnosis> {
        self.diagnose_from(Os::current())
    }

    fn diagnose_from(&self, local: Os) -> Option<ConnectDiagnosis> {
        let port = self.attempts.first()?.addr.port();
        let quic = self.most_telling(TransportKind::Quic);
        let tcp = self.most_telling(TransportKind::Tcp);
        let any = |failure: ConnectFailure| self.attempts.iter().any(|attempt| attempt.failure == failure);
        let hint = if self.attempts.iter().all(|attempt| attempt.failure == ConnectFailure::NetworkDown) {
            Some(ConnectHint::NoNetwork)
        } else if local == Os::MacOs && any(ConnectFailure::NoRoute) {
            Some(ConnectHint::MacosLocalNetwork)
        } else if any(ConnectFailure::Refused) {
            Some(ConnectHint::NotRunning)
        } else if any(ConnectFailure::TimedOut) || any(ConnectFailure::NoRoute) {
            Some(match self.peer_os {
                Some(Os::Linux) => ConnectHint::FirewallLinux,
                Some(Os::Windows) => ConnectHint::FirewallWindows,
                Some(Os::MacOs) => ConnectHint::FirewallMacos,
                None => ConnectHint::Firewall,
            })
        } else {
            None
        };
        Some(ConnectDiagnosis { quic, tcp, port, hint })
    }

    fn most_telling(&self, transport: TransportKind) -> Option<ConnectFailure> {
        self.attempts
            .iter()
            .filter(|attempt| attempt.transport == transport)
            .map(|attempt| attempt.failure)
            .min_by_key(|failure| failure.rank())
    }
}

/// For logs: `quic 172.20.10.3:52000 timed out, tcp 172.20.10.3:52000 no route to host`.
impl fmt::Display for ConnectReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.attempts.is_empty() {
            return formatter.write_str("no attempts");
        }
        for (index, attempt) in self.attempts.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            let transport = match attempt.transport {
                TransportKind::Quic => "quic",
                TransportKind::Tcp => "tcp",
            };
            write!(formatter, "{transport} {} {}", attempt.addr, attempt.failure)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use ConnectFailure::{NetworkDown, NoRoute, Other, Refused, TimedOut};
    use TransportKind::{Quic, Tcp};

    use super::*;

    fn attempt(last_octet: u8, transport: TransportKind, failure: ConnectFailure) -> ConnectAttempt {
        ConnectAttempt { addr: SocketAddr::from((Ipv4Addr::new(172, 20, 10, last_octet), 52000)), transport, failure }
    }

    fn report(peer_os: Option<Os>, attempts: &[(TransportKind, ConnectFailure)]) -> ConnectReport {
        let attempts = attempts.iter().map(|(transport, failure)| attempt(3, *transport, *failure)).collect();
        ConnectReport { attempts, peer_os }
    }

    #[test]
    fn io_errors_are_classified() {
        let of = |kind| ConnectFailure::of(&Error::Io(io::Error::from(kind)));
        assert_eq!(of(io::ErrorKind::TimedOut), TimedOut);
        assert_eq!(of(io::ErrorKind::ConnectionRefused), Refused);
        assert_eq!(of(io::ErrorKind::HostUnreachable), NoRoute);
        assert_eq!(of(io::ErrorKind::NetworkUnreachable), NetworkDown);
        assert_eq!(of(io::ErrorKind::Other), Other);
        assert_eq!(ConnectFailure::of(&Error::ConnectionLost), Other);
    }

    #[test]
    fn no_route_on_macos_points_at_local_network_permission() {
        let report = report(Some(Os::Linux), &[(Quic, TimedOut), (Tcp, NoRoute)]);
        let diagnosis = report.diagnose_from(Os::MacOs).unwrap();
        assert_eq!(diagnosis.quic, Some(TimedOut));
        assert_eq!(diagnosis.tcp, Some(NoRoute));
        assert_eq!(diagnosis.port, 52000);
        assert_eq!(diagnosis.hint, Some(ConnectHint::MacosLocalNetwork));
    }

    #[test]
    fn no_route_elsewhere_points_at_the_peer_firewall() {
        let report = report(Some(Os::Linux), &[(Quic, TimedOut), (Tcp, NoRoute)]);
        assert_eq!(report.diagnose_from(Os::Linux).unwrap().hint, Some(ConnectHint::FirewallLinux));
    }

    #[test]
    fn timeouts_name_the_firewall_of_the_peer_os() {
        let hint = |peer_os| report(peer_os, &[(Quic, TimedOut), (Tcp, TimedOut)]).diagnose_from(Os::Linux)?.hint;
        assert_eq!(hint(Some(Os::Windows)), Some(ConnectHint::FirewallWindows));
        assert_eq!(hint(Some(Os::MacOs)), Some(ConnectHint::FirewallMacos));
        assert_eq!(hint(Some(Os::Linux)), Some(ConnectHint::FirewallLinux));
        assert_eq!(hint(None), Some(ConnectHint::Firewall));
    }

    #[test]
    fn refused_means_lanshare_is_not_running() {
        let report = report(Some(Os::Windows), &[(Quic, TimedOut), (Tcp, Refused)]);
        assert_eq!(report.diagnose_from(Os::Windows).unwrap().hint, Some(ConnectHint::NotRunning));
    }

    #[test]
    fn only_network_down_means_offline() {
        let report = report(None, &[(Quic, NetworkDown), (Tcp, NetworkDown)]);
        assert_eq!(report.diagnose_from(Os::MacOs).unwrap().hint, Some(ConnectHint::NoNetwork));
    }

    #[test]
    fn the_most_telling_failure_wins_across_addresses() {
        let report = ConnectReport {
            attempts: vec![attempt(9, Tcp, TimedOut), attempt(3, Tcp, Refused), attempt(3, Quic, Other)],
            peer_os: None,
        };
        let diagnosis = report.diagnose_from(Os::Linux).unwrap();
        assert_eq!(diagnosis.tcp, Some(Refused));
        assert_eq!(diagnosis.quic, Some(Other));
    }

    #[test]
    fn nothing_attempted_has_no_diagnosis() {
        assert_eq!(ConnectReport::default().diagnose(), None);
    }

    #[test]
    fn display_lists_every_attempt() {
        let report = report(None, &[(Quic, TimedOut), (Tcp, NoRoute)]);
        assert_eq!(report.to_string(), "quic 172.20.10.3:52000 timed out, tcp 172.20.10.3:52000 no route to host");
    }
}
