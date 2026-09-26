//! Notices network changes (Wi-Fi switch, VPN up, cable plugged in) so the realtime socket
//! reconnects at once instead of waiting out its backoff or the 60 s silence timeout.
//!
//! Portable and dependency-free: it asks the OS which local address would route to a
//! TEST-NET address (a UDP `connect` sends no packets) and reports when that changes.

use std::net::{IpAddr, UdpSocket};

/// The local addresses the OS would use for IPv4 and IPv6 traffic right now.
pub fn sample() -> (Option<IpAddr>, Option<IpAddr>) {
    fn route(bind: &str, target: &str) -> Option<IpAddr> {
        let socket = UdpSocket::bind(bind).ok()?;
        socket.connect(target).ok()?;
        socket.local_addr().ok().map(|a| a.ip())
    }
    // 192.0.2.1 and 2001:db8::1 are documentation ranges: never contacted.
    (
        route("0.0.0.0:0", "192.0.2.1:9"),
        route("[::]:0", "[2001:db8::1]:9"),
    )
}

/// Compares successive samples.
#[derive(Debug, Default)]
pub struct Watch<T> {
    last: Option<T>,
}

impl<T: PartialEq> Watch<T> {
    /// `true` when `sample` differs from the previous one (never for the first sample).
    pub fn changed(&mut self, sample: T) -> bool {
        let changed = self.last.as_ref().is_some_and(|l| *l != sample);
        self.last = Some(sample);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_changes_only() {
        let mut w = Watch::default();
        assert!(!w.changed(1));
        assert!(!w.changed(1));
        assert!(w.changed(2));
        assert!(!w.changed(2));
        assert!(w.changed(1));
    }

    #[test]
    fn sampling_never_fails_loudly() {
        // Whatever the machine's network, sampling returns (possibly empty) addresses.
        let (v4, v6) = sample();
        assert!(v4.is_none_or(|a| a.is_ipv4()));
        assert!(v6.is_none_or(|a| a.is_ipv6()));
    }
}
