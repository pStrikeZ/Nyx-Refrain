//! High-level mDNS device browser and cache integration.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use thiserror::Error;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use crate::cache::DeviceCache;
use crate::device::DiscoveredDevice;
use crate::mdns::{
    MDNS_MULTICAST_ADDR, MDNS_PORT, ParsedMdnsRecords, build_mdns_ptr_query, parse_mdns_response,
};

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("I/O error during discovery: {0}")]
    Io(#[from] std::io::Error),

    #[error("no usable network interface found for mDNS")]
    NoUsableInterface,

    #[error("specified interface '{0}' not found on system")]
    InterfaceNotFound(String),
}

/// Options to customize device discovery browsing.
#[derive(Debug, Clone)]
pub struct DiscoveryOptions {
    /// Maximum duration to listen for mDNS responses.
    pub timeout: Duration,
    /// Specific network interface name to bind to (e.g. "eth0" or "Wi-Fi").
    pub interface: Option<String>,
    /// Whether to fall back to `devices.toml` cache on timeout / no response.
    pub cache_fallback: bool,
    /// Print raw TXT records during discovery.
    pub dump_txt: bool,
    /// Custom path to `devices.toml` cache file (primarily for testing).
    pub cache_path: Option<std::path::PathBuf>,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            interface: None,
            cache_fallback: true,
            dump_txt: false,
            cache_path: None,
        }
    }
}

/// Interfaces to query: the named one, or every usable physical interface with IPv4
/// (falling back to any non-loopback interface with IPv4).
fn candidate_interfaces(
    options: &DiscoveryOptions,
) -> Result<Vec<(String, Ipv4Addr)>, DiscoveryError> {
    if let Some(ref name) = options.interface {
        let all = netutil::enumerate_interfaces()?;
        let found = all
            .iter()
            .find(|i| i.name == *name || i.friendly_name.as_deref() == Some(name))
            .ok_or_else(|| DiscoveryError::InterfaceNotFound(name.clone()))?;
        let ip = found
            .primary_ipv4()
            .ok_or(DiscoveryError::NoUsableInterface)?;
        return Ok(vec![(found.name.clone(), ip)]);
    }
    let classified =
        netutil::enumerate_and_classify_interfaces(&netutil::ClassificationConfig::default())?;
    let mut out: Vec<(String, Ipv4Addr)> = classified
        .iter()
        .filter(|c| c.is_usable_physical)
        .filter_map(|c| c.primary_ipv4().map(|ip| (c.iface.name.clone(), ip)))
        .collect();
    if out.is_empty() {
        out = classified
            .iter()
            .filter_map(|c| c.primary_ipv4().map(|ip| (c.iface.name.clone(), ip)))
            .filter(|(_, ip)| !ip.is_loopback())
            .collect();
    }
    Ok(out)
}

/// Query socket for one interface. `IP_MULTICAST_IF` is essential on Windows: multicast
/// egress follows the routing table, not the bound address, so without it the query leaves
/// through a TUN / virtual adapter and no receiver ever sees it.
fn query_socket(ip: Ipv4Addr) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.bind(&SocketAddr::V4(SocketAddrV4::new(ip, 0)).into())?;
    s.set_multicast_if_v4(&ip)?;
    s.set_multicast_ttl_v4(255)?;
    let _ = s.set_multicast_loop_v4(true);
    s.set_nonblocking(true)?;
    Ok(s.into())
}

/// Best-effort listener on 5353 for responders that answer by multicast despite the QU bit.
fn multicast_listener(ifaces: &[(String, Ipv4Addr)]) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    let _ = s.set_reuse_port(true);
    s.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MDNS_PORT)).into())?;
    for (_, ip) in ifaces {
        let _ = s.join_multicast_v4(&MDNS_MULTICAST_ADDR, ip);
    }
    s.set_nonblocking(true)?;
    Ok(s.into())
}

/// Discovers AirPlay / RAOP devices on the LAN using mDNS.
pub async fn discover_devices(
    options: &DiscoveryOptions,
) -> Result<Vec<DiscoveredDevice>, DiscoveryError> {
    use std::sync::Arc;

    let ifaces = candidate_interfaces(options)?;
    if ifaces.is_empty() {
        warn!("No active network interface with IPv4 detected");
        if options.cache_fallback {
            let cache = DeviceCache::load_default();
            if !cache.devices.is_empty() {
                info!(
                    "No active interface; returning {} cached devices",
                    cache.devices.len()
                );
                return Ok(cache.devices);
            }
        }
        return Err(DiscoveryError::NoUsableInterface);
    }
    debug!("mDNS discovery on interfaces: {ifaces:?}");

    let mut query_sockets: Vec<Arc<UdpSocket>> = Vec::new();
    for (name, ip) in &ifaces {
        match query_socket(*ip).and_then(UdpSocket::from_std) {
            Ok(s) => query_sockets.push(Arc::new(s)),
            Err(e) => warn!("mDNS query socket on {name} ({ip}) failed: {e}"),
        }
    }
    if query_sockets.is_empty() {
        return Err(DiscoveryError::NoUsableInterface);
    }
    let mut recv_sockets = query_sockets.clone();
    match multicast_listener(&ifaces).and_then(UdpSocket::from_std) {
        Ok(l) => recv_sockets.push(Arc::new(l)),
        Err(e) => debug!("mDNS 5353 listener unavailable (continuing with unicast replies): {e}"),
    }

    let deadline = tokio::time::Instant::now() + options.timeout;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let mut tasks = Vec::new();
    for sock in &recv_sockets {
        let (sock, tx) = (sock.clone(), tx.clone());
        tasks.push(tokio::spawn(async move {
            let mut buf = vec![0u8; 9000];
            loop {
                match tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
                    Ok(Ok((len, _))) => {
                        let _ = tx.send(buf[..len].to_vec());
                    }
                    // Windows reports ICMP errors (WSAECONNRESET) on UDP recv; keep listening.
                    Ok(Err(e)) => {
                        debug!("mDNS recv error (ignored): {e}");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(_) => break,
                }
            }
        }));
    }
    drop(tx);

    // Send the query on every interface, repeated for lossy Wi-Fi links.
    let query_bytes = build_mdns_ptr_query(&["_raop._tcp.local", "_airplay._tcp.local"]);
    let mdns_dest = SocketAddr::V4(SocketAddrV4::new(MDNS_MULTICAST_ADDR, MDNS_PORT));
    let senders = query_sockets.clone();
    let send_task = tokio::spawn(async move {
        for delay_ms in [0u64, 500, 1500] {
            tokio::time::sleep_until(tokio::time::Instant::now() + Duration::from_millis(delay_ms))
                .await;
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            for s in &senders {
                if let Err(e) = s.send_to(&query_bytes, mdns_dest).await {
                    debug!("mDNS query send failed: {e}");
                }
            }
        }
    });

    let mut combined_records = ParsedMdnsRecords::default();
    while let Some(pkt) = rx.recv().await {
        if let Some(records) = parse_mdns_response(&pkt) {
            combined_records.merge(records);
        }
    }
    send_task.abort();
    for t in tasks {
        let _ = t.await;
    }

    // 5. Assemble discovered devices
    let discovered = combined_records.assemble_devices();

    let load_cache = || match options.cache_path {
        Some(ref p) => DeviceCache::load_from_path(p).unwrap_or_default(),
        None => DeviceCache::load_default(),
    };
    let save_cache = |c: &DeviceCache| match options.cache_path {
        Some(ref p) => c.save_to_path(p),
        None => c.save_default(),
    };

    if !discovered.is_empty() {
        debug!("Discovered {} device(s) via mDNS", discovered.len());
        // Update persistent cache
        let mut cache = load_cache();
        cache.update(&discovered);
        if let Err(e) = save_cache(&cache) {
            warn!("Failed to update devices.toml cache: {e}");
        }
        return Ok(discovered);
    }

    // 6. Fallback to cache if enabled
    if options.cache_fallback {
        let cache = load_cache();
        if !cache.devices.is_empty() {
            info!(
                "mDNS resolution timed out; falling back to {} cached device(s) from devices.toml",
                cache.devices.len()
            );
            return Ok(cache.devices);
        }
    }

    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use raop::capabilities::DeviceCapabilities;
    use std::collections::HashMap;
    use std::fs;

    #[tokio::test]
    async fn test_mdns_unavailable_cache_fallback() {
        let temp_dir = std::env::temp_dir().join("nyx_refrain_discovery_fallback_test");
        let cache_file = temp_dir.join("devices.toml");

        let mut cache = DeviceCache::default();
        let cached_dev = DiscoveredDevice {
            name: "Living Room".to_string(),
            instance_name: "AABBCCDDEEFF@Living Room._raop._tcp.local.".to_string(),
            ip: Ipv4Addr::new(192, 0, 2, 106),
            port: 7000,
            host: Some("living-room.local.".to_string()),
            mac: Some("AA:BB:CC:DD:EE:FF".to_string()),
            capabilities: DeviceCapabilities::from_txt_strings(["cn=0,1,2,3", "et=0,3,5"]),
            txt_records: HashMap::from([("cn".to_string(), "0,1,2,3".to_string())]),
            last_seen_epoch_secs: 1727200000,
        };
        cache.update(&[cached_dev]);
        cache
            .save_to_path(&cache_file)
            .expect("Failed to write test cache");

        // Simulate zero timeout or immediate timeout with cache fallback enabled
        let opts = DiscoveryOptions {
            timeout: Duration::from_millis(1),
            interface: None,
            cache_fallback: true,
            dump_txt: false,
            cache_path: Some(cache_file.clone()),
        };

        let result = discover_devices(&opts).await;
        assert!(result.is_ok());
        let devs = result.unwrap();
        assert_eq!(devs.len(), 1, "Should have fallen back to cached device");
        assert_eq!(devs[0].name, "Living Room");
        assert_eq!(devs[0].ip, Ipv4Addr::new(192, 0, 2, 106));

        // Now test with cache_fallback disabled
        let opts_no_cache = DiscoveryOptions {
            timeout: Duration::from_millis(1),
            interface: None,
            cache_fallback: false,
            dump_txt: false,
            cache_path: Some(cache_file.clone()),
        };

        let result_no_cache = discover_devices(&opts_no_cache).await;
        assert!(result_no_cache.is_ok());
        assert!(result_no_cache.unwrap().is_empty());

        let _ = fs::remove_file(&cache_file);
        let _ = fs::remove_dir(&temp_dir);
    }
}
