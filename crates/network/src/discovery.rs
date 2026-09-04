use std::collections::HashMap;
use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mdns_sd::{
    Receiver, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo, TryRecvError,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// DNS-SD service type used by Superspace peers.
pub const SERVICE_TYPE: &str = "_superspace._tcp.local.";
const PROTOCOL_VERSION: &str = "1";
const BROADCAST_PORT: u16 = 43_869;
const MAX_BEACON_BYTES: usize = 1_024;
const BEACON_INTERVAL: Duration = Duration::from_secs(1);

/// A validated peer announcement resolved to reachable local addresses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NearbyDevice {
    /// Stable installation identity.
    pub id: Uuid,
    /// User-visible device name.
    pub name: String,
    /// Short BLAKE3 fingerprint of the advertised Noise static key.
    pub key_fingerprint: String,
    /// Candidate QUIC endpoints, including IPv6 scope when required.
    pub addresses: Vec<SocketAddr>,
}

/// Change emitted by the non-blocking discovery poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryEvent {
    /// A peer was resolved or its addresses changed.
    Resolved(NearbyDevice),
    /// A DNS-SD instance disappeared from the local network.
    Removed(String),
}

/// Active mDNS advertiser and browser for nearby Superspace peers.
pub struct NearbyDiscovery {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    registered_fullname: String,
    broadcast: Option<BroadcastDiscovery>,
}

struct BroadcastDiscovery {
    socket: UdpSocket,
    local_id: Uuid,
    beacon: Vec<u8>,
    last_sent: Mutex<Option<Instant>>,
}

#[derive(Serialize, Deserialize)]
struct BroadcastBeacon {
    id: Uuid,
    name: String,
    version: u16,
    pairing_port: u16,
    key_fingerprint: String,
}

impl NearbyDiscovery {
    /// Advertise this device and begin browsing for peers.
    ///
    /// `host_label` must be a single DNS label. Interface addresses are tracked automatically as
    /// Wi-Fi, Ethernet, and VPN state changes.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError`] for invalid metadata or an mDNS daemon failure.
    pub fn start(
        id: Uuid,
        name: &str,
        host_label: &str,
        port: u16,
        public_key: &[u8],
    ) -> Result<Self, DiscoveryError> {
        if port == 0 || name.trim().is_empty() || !valid_dns_label(host_label) {
            return Err(DiscoveryError::InvalidAdvertisement);
        }
        let daemon = ServiceDaemon::new()?;
        let info = service_info(id, name, host_label, port, public_key)?;
        let registered_fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        let events = daemon.browse(SERVICE_TYPE)?;
        let broadcast = BroadcastDiscovery::start(id, name, port, public_key).ok();
        Ok(Self {
            daemon,
            events,
            registered_fullname,
            broadcast,
        })
    }

    /// Drain currently queued discovery changes without blocking the UI thread.
    ///
    /// Malformed or incompatible announcements are ignored rather than surfaced as trusted peers.
    #[must_use]
    pub fn poll(&self) -> Vec<DiscoveryEvent> {
        let mut output = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    if let Some(device) = parse_resolved(&info) {
                        output.push(DiscoveryEvent::Resolved(device));
                    }
                }
                Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                    output.push(DiscoveryEvent::Removed(fullname));
                }
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if let Some(broadcast) = &self.broadcast {
            output.extend(broadcast.poll());
        }
        output
    }

    /// Stop browsing and send a goodbye record before shutdown.
    pub fn stop(self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.unregister(&self.registered_fullname);
        let _ = self.daemon.shutdown();
    }
}

impl BroadcastDiscovery {
    fn start(id: Uuid, name: &str, port: u16, public_key: &[u8]) -> std::io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, BROADCAST_PORT))?;
        socket.set_broadcast(true)?;
        socket.set_nonblocking(true)?;
        let beacon = BroadcastBeacon {
            id,
            name: name.to_owned(),
            version: 1,
            pairing_port: port,
            key_fingerprint: fingerprint(public_key),
        };
        let mut bytes = Vec::new();
        ciborium::into_writer(&beacon, &mut bytes)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self {
            socket,
            local_id: id,
            beacon: bytes,
            last_sent: Mutex::new(None),
        })
    }

    fn poll(&self) -> Vec<DiscoveryEvent> {
        let now = Instant::now();
        if let Ok(mut last_sent) = self.last_sent.lock()
            && last_sent.is_none_or(|last| now.duration_since(last) >= BEACON_INTERVAL)
        {
            let _ = self.socket.send_to(
                &self.beacon,
                SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), BROADCAST_PORT),
            );
            *last_sent = Some(now);
        }
        let mut events = Vec::new();
        let mut bytes = [0_u8; MAX_BEACON_BYTES];
        loop {
            match self.socket.recv_from(&mut bytes) {
                Ok((length, source)) => {
                    if let Some(device) =
                        parse_broadcast_beacon(&bytes[..length], source.ip(), self.local_id)
                    {
                        events.push(DiscoveryEvent::Resolved(device));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        events
    }
}

fn parse_broadcast_beacon(bytes: &[u8], source: IpAddr, local_id: Uuid) -> Option<NearbyDevice> {
    let beacon: BroadcastBeacon = ciborium::from_reader(Cursor::new(bytes)).ok()?;
    if beacon.id.is_nil()
        || beacon.id == local_id
        || beacon.name.trim().is_empty()
        || beacon.name.chars().count() > 128
        || beacon.version != 1
        || beacon.pairing_port == 0
        || beacon.key_fingerprint.len() != 16
        || !beacon
            .key_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(NearbyDevice {
        id: beacon.id,
        name: beacon.name,
        key_fingerprint: beacon.key_fingerprint.to_ascii_lowercase(),
        addresses: vec![SocketAddr::new(source, beacon.pairing_port)],
    })
}

impl Drop for NearbyDiscovery {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.unregister(&self.registered_fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Discovery configuration and daemon failures.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// Device name, host label, port, or public key is unsuitable for advertisement.
    #[error("nearby advertisement is invalid")]
    InvalidAdvertisement,
    /// The platform mDNS daemon could not start or accept an operation.
    #[error("nearby discovery failed")]
    Mdns(#[from] mdns_sd::Error),
}

fn service_info(
    id: Uuid,
    name: &str,
    host_label: &str,
    port: u16,
    public_key: &[u8],
) -> Result<ServiceInfo, DiscoveryError> {
    if public_key.len() != 32 {
        return Err(DiscoveryError::InvalidAdvertisement);
    }
    let fingerprint = fingerprint(public_key);
    let properties = HashMap::from([
        ("id".to_owned(), id.to_string()),
        ("name".to_owned(), name.to_owned()),
        ("v".to_owned(), PROTOCOL_VERSION.to_owned()),
        ("key".to_owned(), fingerprint),
    ]);
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &id.to_string(),
        &format!("{host_label}.local."),
        "",
        port,
        properties,
    )?
    .enable_addr_auto();
    Ok(info)
}

fn parse_resolved(info: &ResolvedService) -> Option<NearbyDevice> {
    if info.get_property_val_str("v")? != PROTOCOL_VERSION {
        return None;
    }
    let id = info.get_property_val_str("id")?.parse().ok()?;
    let name = info.get_property_val_str("name")?.trim();
    let key_fingerprint = info.get_property_val_str("key")?;
    if name.is_empty()
        || key_fingerprint.len() != 16
        || !key_fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let mut addresses = info
        .get_addresses()
        .iter()
        .filter_map(|address| match address {
            ScopedIp::V4(value) => Some(SocketAddr::new((*value.addr()).into(), info.get_port())),
            ScopedIp::V6(value) => Some(SocketAddr::V6(SocketAddrV6::new(
                *value.addr(),
                info.get_port(),
                0,
                value.scope_id().index,
            ))),
            _ => None,
        })
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    (!addresses.is_empty()).then(|| NearbyDevice {
        id,
        name: name.to_owned(),
        key_fingerprint: key_fingerprint.to_ascii_lowercase(),
        addresses,
    })
}

fn fingerprint(public_key: &[u8]) -> String {
    blake3::hash(public_key).as_bytes()[..8].iter().fold(
        String::with_capacity(16),
        |mut output, byte| {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
            output
        },
    )
}

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn advertisement_contains_only_public_identity_metadata() {
        let id = Uuid::new_v4();
        let key = [7_u8; 32];
        let info = service_info(id, "Work Mac", "work-mac", 43120, &key).expect("service info");
        assert_eq!(
            info.get_property_val_str("id"),
            Some(id.to_string().as_str())
        );
        assert_eq!(info.get_property_val_str("name"), Some("Work Mac"));
        assert_eq!(info.get_property_val_str("v"), Some("1"));
        assert_eq!(
            info.get_property_val_str("key"),
            Some(fingerprint(&key).as_str())
        );
        assert!(!format!("{info:?}").contains(&"07".repeat(32)));
    }

    #[test]
    fn rejects_invalid_dns_labels_ports_and_keys() {
        let id = Uuid::new_v4();
        assert!(service_info(id, "Name", "host", 1, &[0; 31]).is_err());
        assert!(!valid_dns_label("-host"));
        assert!(!valid_dns_label("host.local"));
        assert!(valid_dns_label("macbook-pro"));
    }

    #[test]
    fn broadcast_beacons_are_bounded_validated_and_resolved_from_the_sender() {
        let local_id = Uuid::new_v4();
        let remote_id = Uuid::new_v4();
        let beacon = BroadcastBeacon {
            id: remote_id,
            name: "Nearby Mac".into(),
            version: 1,
            pairing_port: 43_870,
            key_fingerprint: "0123456789abcdef".into(),
        };
        let mut bytes = Vec::new();
        ciborium::into_writer(&beacon, &mut bytes).expect("encode beacon");
        let device =
            parse_broadcast_beacon(&bytes, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7)), local_id)
                .expect("valid beacon");
        assert_eq!(device.id, remote_id);
        assert_eq!(device.addresses, ["192.168.1.7:43870".parse().unwrap()]);

        let invalid = BroadcastBeacon {
            version: 9,
            ..beacon
        };
        bytes.clear();
        ciborium::into_writer(&invalid, &mut bytes).expect("encode invalid beacon");
        assert!(
            parse_broadcast_beacon(&bytes, IpAddr::V4(Ipv4Addr::LOCALHOST), local_id).is_none()
        );
    }

    #[test]
    #[ignore = "requires working multicast on a host network"]
    fn multicast_daemons_discover_each_other() {
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let first = NearbyDiscovery::start(first_id, "First", "superspace-first", 43_121, &[1; 32])
            .expect("first daemon");
        let second =
            NearbyDiscovery::start(second_id, "Second", "superspace-second", 43_122, &[2; 32])
                .expect("second daemon");
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut first_saw_second = false;
        let mut second_saw_first = false;
        while Instant::now() < deadline && !(first_saw_second && second_saw_first) {
            first_saw_second |= first.poll().iter().any(
                |event| matches!(event, DiscoveryEvent::Resolved(device) if device.id == second_id),
            );
            second_saw_first |= second.poll().iter().any(
                |event| matches!(event, DiscoveryEvent::Resolved(device) if device.id == first_id),
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(first_saw_second && second_saw_first);
    }
}
