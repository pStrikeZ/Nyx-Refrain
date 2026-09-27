//! Manual target specification skipping mDNS resolution.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

use raop::capabilities::DeviceCapabilities;

use crate::device::DiscoveredDevice;

/// Builds a `DiscoveredDevice` from manual command-line target parameters,
/// completely skipping mDNS queries.
pub fn build_manual_target(
    target_str: &str,
    name: Option<&str>,
    et_str: Option<&str>,
    cn_str: Option<&str>,
) -> Result<DiscoveredDevice, String> {
    let (ip, port) = parse_target_addr(target_str)?;

    let target_name = name.map_or_else(|| ip.to_string(), str::to_string);
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let et = et_str.unwrap_or("0,3,5");
    let cn = cn_str.unwrap_or("0,1,2,3");

    let mut txt_strings = Vec::new();
    txt_strings.push(format!("et={et}"));
    txt_strings.push(format!("cn={cn}"));
    txt_strings.push("tp=UDP".to_string());
    txt_strings.push("am=AudioAccessory5,1".to_string());
    txt_strings.push("vn=65537".to_string());

    let capabilities = DeviceCapabilities::from_txt_strings(txt_strings.iter().map(|s| s.as_str()));

    let mut txt_records = HashMap::new();
    txt_records.insert("et".to_string(), et.to_string());
    txt_records.insert("cn".to_string(), cn.to_string());
    txt_records.insert("tp".to_string(), "UDP".to_string());
    txt_records.insert("am".to_string(), "AudioAccessory5,1".to_string());
    txt_records.insert("vn".to_string(), "65537".to_string());

    let instance_name = format!("{target_name}._raop._tcp.local.");

    Ok(DiscoveredDevice {
        name: target_name,
        instance_name,
        ip,
        port,
        host: None,
        mac: None,
        capabilities,
        txt_records,
        last_seen_epoch_secs: now_epoch,
    })
}

/// Parses an IPv4 or IPv4:port string. Defaults to port 7000 if not specified.
pub fn parse_target_addr(target_str: &str) -> Result<(Ipv4Addr, u16), String> {
    if let Ok(sock_addr) = target_str.parse::<SocketAddr>() {
        match sock_addr {
            SocketAddr::V4(v4) => Ok((*v4.ip(), v4.port())),
            SocketAddr::V6(_) => Err("IPv6 targets are not currently supported".to_string()),
        }
    } else if let Some((ip_part, port_part)) = target_str.split_once(':') {
        let ip: Ipv4Addr = ip_part
            .parse()
            .map_err(|e| format!("Invalid IPv4 address '{ip_part}': {e}"))?;
        let port: u16 = port_part
            .parse()
            .map_err(|e| format!("Invalid port '{port_part}': {e}"))?;
        Ok((ip, port))
    } else {
        let ip: Ipv4Addr = target_str
            .parse()
            .map_err(|e| format!("Invalid IPv4 address '{target_str}': {e}"))?;
        Ok((ip, 7000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_target_addr() {
        let (ip, port) = parse_target_addr("192.0.2.106").unwrap();
        assert_eq!(ip, Ipv4Addr::new(192, 0, 2, 106));
        assert_eq!(port, 7000);

        let (ip2, port2) = parse_target_addr("192.168.1.50:5000").unwrap();
        assert_eq!(ip2, Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(port2, 5000);
    }

    #[test]
    fn test_manual_target_construction() {
        let dev = build_manual_target("192.0.2.106", Some("Living Room"), Some("0,1"), Some("1"))
            .unwrap();
        assert_eq!(dev.name, "Living Room");
        assert_eq!(dev.ip, Ipv4Addr::new(192, 0, 2, 106));
        assert_eq!(dev.port, 7000);
        assert_eq!(dev.capabilities.encryption_types, vec![0, 1]);
        assert_eq!(dev.capabilities.codecs, vec![1]);
    }
}
