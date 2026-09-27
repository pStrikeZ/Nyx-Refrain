//! Network interface data types and representations.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::Ipv4Addr;

/// An IPv4 address with subnet prefix length (CIDR notation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ipv4Net {
    pub ip: Ipv4Addr,
    pub prefix_len: u8,
}

impl Ipv4Net {
    /// Creates a new `Ipv4Net` with sanity clamping on `prefix_len` (max 32).
    pub fn new(ip: Ipv4Addr, prefix_len: u8) -> Self {
        Self {
            ip,
            prefix_len: prefix_len.min(32),
        }
    }

    /// Computes the netmask corresponding to `prefix_len`.
    pub fn netmask(&self) -> Ipv4Addr {
        if self.prefix_len == 0 {
            Ipv4Addr::new(0, 0, 0, 0)
        } else {
            let mask = (!0u32)
                .checked_shl(32 - self.prefix_len as u32)
                .unwrap_or(0);
            Ipv4Addr::from(mask)
        }
    }

    /// Computes the network base address.
    pub fn network(&self) -> Ipv4Addr {
        let ip_u32 = u32::from(self.ip);
        let mask_u32 = u32::from(self.netmask());
        Ipv4Addr::from(ip_u32 & mask_u32)
    }

    /// Checks if a given target IPv4 address falls within this subnet.
    pub fn contains(&self, target: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask());
        (u32::from(self.ip) & mask) == (u32::from(target) & mask)
    }
}

impl fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix_len)
    }
}

/// Interface type, mapped to Windows MIB_IF_TYPE values or Linux equivalents.
///
/// Upstream references:
/// - Windows SDK `iptypes.h` / `IfType`
/// - Linux `/sys/class/net/<iface>/type` (ARPHRD_*)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InterfaceType {
    /// Physical wired Ethernet (Windows IfType 6: IF_TYPE_ETHERNET_CSMACD).
    Ethernet,
    /// Physical wireless 802.11 (Windows IfType 71: IF_TYPE_IEEE80211).
    Wifi,
    /// Software loopback (Windows IfType 24: IF_TYPE_SOFTWARE_LOOPBACK).
    Loopback,
    /// Proprietary virtual network interface (Windows IfType 53: IF_TYPE_PROP_VIRTUAL).
    ProprietaryVirtual,
    /// Tunnel interface (Windows IfType 131: IF_TYPE_TUNNEL).
    Tunnel,
    /// Other / unrecognized interface type with numeric code.
    Other(u32),
}

impl InterfaceType {
    /// Returns true if this interface type represents a physical adapter
    /// (Ethernet or Wi-Fi).
    pub fn is_physical(&self) -> bool {
        matches!(self, InterfaceType::Ethernet | InterfaceType::Wifi)
    }

    /// Returns the raw Windows `IfType` integer if known.
    pub fn if_type_code(&self) -> u32 {
        match self {
            InterfaceType::Ethernet => 6,
            InterfaceType::Wifi => 71,
            InterfaceType::Loopback => 24,
            InterfaceType::ProprietaryVirtual => 53,
            InterfaceType::Tunnel => 131,
            InterfaceType::Other(code) => *code,
        }
    }

    /// Converts a Windows `IfType` integer to `InterfaceType`.
    pub fn from_windows_if_type(if_type: u32) -> Self {
        match if_type {
            6 => InterfaceType::Ethernet,
            71 => InterfaceType::Wifi,
            24 => InterfaceType::Loopback,
            53 => InterfaceType::ProprietaryVirtual,
            131 => InterfaceType::Tunnel,
            other => InterfaceType::Other(other),
        }
    }
}

impl fmt::Display for InterfaceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterfaceType::Ethernet => write!(f, "Ethernet (IfType 6)"),
            InterfaceType::Wifi => write!(f, "Wi-Fi (IfType 71)"),
            InterfaceType::Loopback => write!(f, "Loopback (IfType 24)"),
            InterfaceType::ProprietaryVirtual => write!(f, "Virtual (IfType 53)"),
            InterfaceType::Tunnel => write!(f, "Tunnel (IfType 131)"),
            InterfaceType::Other(code) => write!(f, "Other (IfType {code})"),
        }
    }
}

/// Metadata and addresses for an enumerated network interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    /// System interface identifier (e.g. "eth0" on Linux, or adapter GUID on Windows).
    pub name: String,
    /// User-friendly name (e.g. "Ethernet", "Wi-Fi" on Windows, or interface name on Linux).
    pub friendly_name: Option<String>,
    /// Adapter description string (if provided by OS).
    pub description: Option<String>,
    /// Operating system interface index (if_index).
    pub index: u32,
    /// Associated IPv4 addresses with subnet prefixes.
    pub ipv4_nets: Vec<Ipv4Net>,
    /// Classification of interface type.
    pub if_type: InterfaceType,
    /// True if this is a loopback adapter.
    pub is_loopback: bool,
    /// True if the interface link state is UP.
    pub is_up: bool,
}

impl InterfaceInfo {
    /// Returns the best human-readable name for display.
    pub fn display_name(&self) -> &str {
        if let Some(ref friendly) = self.friendly_name
            && !friendly.is_empty()
        {
            return friendly.as_str();
        }
        &self.name
    }

    /// Returns the first assigned IPv4 address, if any.
    pub fn primary_ipv4(&self) -> Option<Ipv4Addr> {
        self.ipv4_nets.first().map(|net| net.ip)
    }

    /// Returns true if any assigned subnet contains the target IPv4 address.
    pub fn contains_ipv4(&self, target: Ipv4Addr) -> bool {
        self.ipv4_nets.iter().any(|net| net.contains(target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipv4net_subnet_contains() {
        let net = Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 21), 24);
        assert_eq!(net.netmask(), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(net.network(), Ipv4Addr::new(10, 0, 0, 0));

        assert!(net.contains(Ipv4Addr::new(10, 0, 0, 106)));
        assert!(net.contains(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!net.contains(Ipv4Addr::new(10, 0, 1, 106)));
        assert!(!net.contains(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn test_ipv4net_various_prefix_lengths() {
        let net_16 = Ipv4Net::new(Ipv4Addr::new(172, 16, 5, 10), 16);
        assert_eq!(net_16.netmask(), Ipv4Addr::new(255, 255, 0, 0));
        assert!(net_16.contains(Ipv4Addr::new(172, 16, 250, 1)));
        assert!(!net_16.contains(Ipv4Addr::new(172, 17, 1, 1)));

        let net_32 = Ipv4Net::new(Ipv4Addr::new(192, 168, 1, 50), 32);
        assert_eq!(net_32.netmask(), Ipv4Addr::new(255, 255, 255, 255));
        assert!(net_32.contains(Ipv4Addr::new(192, 168, 1, 50)));
        assert!(!net_32.contains(Ipv4Addr::new(192, 168, 1, 51)));
    }

    #[test]
    fn test_interface_type_physical() {
        assert!(InterfaceType::Ethernet.is_physical());
        assert!(InterfaceType::Wifi.is_physical());
        assert!(!InterfaceType::Loopback.is_physical());
        assert!(!InterfaceType::ProprietaryVirtual.is_physical());
        assert!(!InterfaceType::Tunnel.is_physical());
        assert!(!InterfaceType::Other(1).is_physical());
    }

    #[test]
    fn test_interface_type_codes_and_conversions() {
        let types = [
            (InterfaceType::Ethernet, 6),
            (InterfaceType::Wifi, 71),
            (InterfaceType::Loopback, 24),
            (InterfaceType::ProprietaryVirtual, 53),
            (InterfaceType::Tunnel, 131),
            (InterfaceType::Other(999), 999),
        ];

        for (if_type, code) in types {
            assert_eq!(if_type.if_type_code(), code);
            assert_eq!(InterfaceType::from_windows_if_type(code), if_type);
            assert!(!format!("{if_type}").is_empty());
        }
    }

    #[test]
    fn test_ipv4net_display() {
        let net = Ipv4Net::new(Ipv4Addr::new(192, 168, 1, 100), 24);
        assert_eq!(format!("{net}"), "192.168.1.100/24");
    }

    #[test]
    fn test_interface_info_helpers() {
        let empty_info = InterfaceInfo {
            name: "eth0".to_string(),
            friendly_name: None,
            description: None,
            index: 1,
            ipv4_nets: vec![],
            if_type: InterfaceType::Ethernet,
            is_loopback: false,
            is_up: true,
        };
        assert_eq!(empty_info.display_name(), "eth0");
        assert_eq!(empty_info.primary_ipv4(), None);
        assert!(!empty_info.contains_ipv4(Ipv4Addr::new(10, 0, 0, 1)));

        let full_info = InterfaceInfo {
            name: "iface-guid-123".to_string(),
            friendly_name: Some("Wi-Fi".to_string()),
            description: Some("Intel Wi-Fi 6 AX200".to_string()),
            index: 2,
            ipv4_nets: vec![
                Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 21), 24),
                Ipv4Net::new(Ipv4Addr::new(192, 168, 122, 1), 24),
            ],
            if_type: InterfaceType::Wifi,
            is_loopback: false,
            is_up: true,
        };
        assert_eq!(full_info.display_name(), "Wi-Fi");
        assert_eq!(full_info.primary_ipv4(), Some(Ipv4Addr::new(10, 0, 0, 21)));
        assert!(full_info.contains_ipv4(Ipv4Addr::new(10, 0, 0, 106)));
        assert!(full_info.contains_ipv4(Ipv4Addr::new(192, 168, 122, 50)));
        assert!(!full_info.contains_ipv4(Ipv4Addr::new(172, 16, 0, 1)));

        let whitespace_info = InterfaceInfo {
            name: "eth1".to_string(),
            friendly_name: Some("".to_string()),
            description: None,
            index: 3,
            ipv4_nets: vec![],
            if_type: InterfaceType::Ethernet,
            is_loopback: false,
            is_up: true,
        };
        assert_eq!(whitespace_info.display_name(), "eth1");
    }
}
