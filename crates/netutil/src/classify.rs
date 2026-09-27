//! Network interface classification and selection rules.
//!
//! Enforces physical vs virtual adapter separation rules on Windows and Linux,
//! distinguishing real Ethernet/Wi-Fi hardware from TUN, Clash, Hyper-V, Docker,
//! and VPN virtual adapters. Supports configuration overrides and target IP subnet
//! auto-selection.

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

use crate::interface::{InterfaceInfo, InterfaceType};

/// Windows blacklist patterns for adapter name and description:
/// `wintun|tun|tap|clash|meta|mihomo|sing|wireguard|zerotier|tailscale|vEthernet|VMware|VirtualBox|Hyper-V`
pub const WINDOWS_NAME_BLACKLIST: &[&str] = &[
    "wintun",
    "tun",
    "tap",
    "clash",
    "meta",
    "mihomo",
    "sing",
    "wireguard",
    "zerotier",
    "tailscale",
    "vethernet",
    "vmware",
    "virtualbox",
    "hyper-v",
];

/// Linux blacklist interface name prefixes:
/// `lo`, `docker*`, `br-*`, `veth*`, `virbr*`, `tun*`, `tap*`, `wg*`, `tailscale*`, `zt*`
pub const LINUX_PREFIX_BLACKLIST: &[&str] = &[
    "lo",
    "docker",
    "br-",
    "veth",
    "virbr",
    "tun",
    "tap",
    "wg",
    "tailscale",
    "zt",
];

/// Configuration for custom classification rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassificationConfig {
    /// Whitelisted interface names that bypass virtual exclusions.
    #[serde(default)]
    pub allowed_interfaces: Vec<String>,
    /// Explicitly excluded interface names.
    #[serde(default)]
    pub excluded_interfaces: Vec<String>,
    /// Additional case-insensitive blacklist substrings.
    #[serde(default)]
    pub extra_blacklist_patterns: Vec<String>,
}

/// An interface coupled with its usability classification and human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifiedInterface {
    pub iface: InterfaceInfo,
    pub is_usable_physical: bool,
    pub reason: String,
}

impl ClassifiedInterface {
    /// Returns the interface name or friendly name.
    pub fn display_name(&self) -> &str {
        self.iface.display_name()
    }

    /// Returns the primary IPv4 address if assigned.
    pub fn primary_ipv4(&self) -> Option<Ipv4Addr> {
        self.iface.primary_ipv4()
    }
}

/// Classifies an interface as usable physical or excluded according to interface selection rules.
pub fn classify_interface(
    iface: &InterfaceInfo,
    config: &ClassificationConfig,
) -> ClassifiedInterface {
    let lower_name = iface.name.to_lowercase();
    let lower_friendly = iface.friendly_name.as_deref().unwrap_or("").to_lowercase();
    let lower_desc = iface.description.as_deref().unwrap_or("").to_lowercase();

    // 1. Explicit user whitelist overrides
    for allowed in &config.allowed_interfaces {
        let allowed_lower = allowed.to_lowercase();
        if lower_name == allowed_lower || lower_friendly == allowed_lower {
            return ClassifiedInterface {
                iface: iface.clone(),
                is_usable_physical: true,
                reason: format!("Explicitly whitelisted in configuration ('{allowed}')"),
            };
        }
    }

    // 2. Explicit user blacklist
    for excluded in &config.excluded_interfaces {
        let excluded_lower = excluded.to_lowercase();
        if lower_name == excluded_lower || lower_friendly == excluded_lower {
            return ClassifiedInterface {
                iface: iface.clone(),
                is_usable_physical: false,
                reason: format!("Explicitly excluded in configuration ('{excluded}')"),
            };
        }
    }

    // 3. Loopback exclusion
    if iface.is_loopback || iface.if_type == InterfaceType::Loopback || lower_name == "lo" {
        return ClassifiedInterface {
            iface: iface.clone(),
            is_usable_physical: false,
            reason: "Loopback interface excluded".to_string(),
        };
    }

    // 4. Windows IfType exclusions (53: proprietary virtual, 131: tunnel)
    match iface.if_type {
        InterfaceType::ProprietaryVirtual => {
            return ClassifiedInterface {
                iface: iface.clone(),
                is_usable_physical: false,
                reason: "Windows: IfType 53 (Proprietary Virtual) excluded".to_string(),
            };
        }
        InterfaceType::Tunnel => {
            return ClassifiedInterface {
                iface: iface.clone(),
                is_usable_physical: false,
                reason: "Windows: IfType 131 (Tunnel) excluded".to_string(),
            };
        }
        _ => {}
    }

    // 5. Windows blacklist substring patterns (checked against name, friendly name, and description)
    for &pattern in WINDOWS_NAME_BLACKLIST {
        if lower_name.contains(pattern)
            || lower_friendly.contains(pattern)
            || lower_desc.contains(pattern)
        {
            return ClassifiedInterface {
                iface: iface.clone(),
                is_usable_physical: false,
                reason: format!("Matches virtual adapter blacklist pattern '{pattern}'"),
            };
        }
    }

    // 6. User-provided extra blacklist patterns
    for pattern in &config.extra_blacklist_patterns {
        let p_lower = pattern.to_lowercase();
        if lower_name.contains(&p_lower)
            || lower_friendly.contains(&p_lower)
            || lower_desc.contains(&p_lower)
        {
            return ClassifiedInterface {
                iface: iface.clone(),
                is_usable_physical: false,
                reason: format!("Matches custom blacklist pattern '{pattern}'"),
            };
        }
    }

    // 7. Linux prefix blacklist (checked against interface system name)
    for &prefix in LINUX_PREFIX_BLACKLIST {
        if lower_name.starts_with(prefix) {
            return ClassifiedInterface {
                iface: iface.clone(),
                is_usable_physical: false,
                reason: format!("Matches Linux virtual interface prefix '{prefix}*'"),
            };
        }
    }

    // 8. If interface has no IPv4 addresses assigned, still report it, but note it
    if iface.ipv4_nets.is_empty() {
        return ClassifiedInterface {
            iface: iface.clone(),
            is_usable_physical: false,
            reason: "No IPv4 address assigned".to_string(),
        };
    }

    // Passed all exclusion checks: classified as usable physical adapter
    let type_desc = match iface.if_type {
        InterfaceType::Ethernet => "Ethernet",
        InterfaceType::Wifi => "Wi-Fi",
        _ => "Physical",
    };

    ClassifiedInterface {
        iface: iface.clone(),
        is_usable_physical: true,
        reason: format!("Usable physical adapter ({type_desc})"),
    }
}

/// Automatically selects the best network interface for a given target IP address.
///
/// Priority:
/// 1. A usable physical interface whose IPv4 subnet directly contains `target_ip`.
/// 2. Any other interface whose IPv4 subnet directly contains `target_ip` (with a warning).
/// 3. The first usable physical interface with an active IPv4 address.
/// 4. The first interface with an active IPv4 address.
pub fn select_interface_for_target(
    interfaces: &[ClassifiedInterface],
    target_ip: Ipv4Addr,
) -> Option<&ClassifiedInterface> {
    // 1. Direct subnet match on usable physical interface
    for classified in interfaces {
        if classified.is_usable_physical && classified.iface.contains_ipv4(target_ip) {
            return Some(classified);
        }
    }

    // 2. Direct subnet match on non-physical / other interface
    for classified in interfaces {
        if classified.iface.contains_ipv4(target_ip) {
            tracing::warn!(
                "Target IP {} matched subnet on non-physical interface '{}' ({})",
                target_ip,
                classified.iface.name,
                classified.reason
            );
            return Some(classified);
        }
    }

    // 3. Fallback: first usable physical interface with IPv4
    for classified in interfaces {
        if classified.is_usable_physical && !classified.iface.ipv4_nets.is_empty() {
            tracing::debug!(
                "No subnet matched target IP {}; falling back to first usable physical NIC '{}'",
                target_ip,
                classified.iface.name
            );
            return Some(classified);
        }
    }

    // 4. Ultimate fallback: first interface with IPv4
    interfaces.iter().find(|c| !c.iface.ipv4_nets.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::Ipv4Net;

    fn make_test_iface(
        name: &str,
        friendly: Option<&str>,
        desc: Option<&str>,
        if_type: InterfaceType,
        ip: Option<(Ipv4Addr, u8)>,
    ) -> InterfaceInfo {
        InterfaceInfo {
            name: name.to_string(),
            friendly_name: friendly.map(|s| s.to_string()),
            description: desc.map(|s| s.to_string()),
            index: 1,
            ipv4_nets: ip
                .map(|(a, p)| vec![Ipv4Net::new(a, p)])
                .unwrap_or_default(),
            if_type,
            is_loopback: if_type == InterfaceType::Loopback,
            is_up: true,
        }
    }

    #[test]
    fn test_windows_clash_wintun_excluded() {
        let config = ClassificationConfig::default();

        // 1. wintun adapter with IfType 53
        let iface = make_test_iface(
            "{B12E3C0F-5B60-4919-9FE4-5FE3C096D11B}",
            Some("Clash"),
            Some("Wintun Userspace Tunnel"),
            InterfaceType::ProprietaryVirtual,
            Some((Ipv4Addr::new(198, 18, 0, 1), 15)),
        );
        let classified = classify_interface(&iface, &config);
        assert!(!classified.is_usable_physical);
        assert!(
            classified.reason.contains("53")
                || classified.reason.contains("wintun")
                || classified.reason.contains("clash")
        );

        // 2. Tunnel adapter with IfType 131
        let iface_tunnel = make_test_iface(
            "{A0000000-0000-0000-0000-000000000001}",
            Some("Meta"),
            Some("WireGuard Tunnel"),
            InterfaceType::Tunnel,
            Some((Ipv4Addr::new(10, 8, 0, 2), 24)),
        );
        let classified_tunnel = classify_interface(&iface_tunnel, &config);
        assert!(!classified_tunnel.is_usable_physical);
    }

    #[test]
    fn test_windows_hyperv_vethernet_excluded() {
        let config = ClassificationConfig::default();

        let iface = make_test_iface(
            "{C3456789-1234-5678-9ABC-DEF012345678}",
            Some("vEthernet (Default Switch)"),
            Some("Hyper-V Virtual Ethernet Adapter"),
            InterfaceType::Ethernet, // Hyper-V often masquerades as IfType 6!
            Some((Ipv4Addr::new(172, 25, 112, 1), 20)),
        );
        let classified = classify_interface(&iface, &config);
        assert!(!classified.is_usable_physical);
        assert!(classified.reason.contains("vethernet") || classified.reason.contains("hyper-v"));
    }

    #[test]
    fn test_windows_vmware_virtualbox_excluded() {
        let config = ClassificationConfig::default();

        let iface_vmware = make_test_iface(
            "{D4567890-2345-6789-ABCD-EF0123456789}",
            Some("VMware Network Adapter VMnet1"),
            Some("VMware Virtual Ethernet Adapter for VMnet1"),
            InterfaceType::Ethernet,
            Some((Ipv4Addr::new(192, 168, 120, 1), 24)),
        );
        assert!(!classify_interface(&iface_vmware, &config).is_usable_physical);

        let iface_vbox = make_test_iface(
            "{E5678901-3456-789A-BCDE-F0123456789A}",
            Some("VirtualBox Host-Only Network"),
            Some("VirtualBox Host-Only Ethernet Adapter"),
            InterfaceType::Ethernet,
            Some((Ipv4Addr::new(192, 168, 56, 1), 24)),
        );
        assert!(!classify_interface(&iface_vbox, &config).is_usable_physical);
    }

    #[test]
    fn test_physical_ethernet_and_wifi_accepted() {
        let config = ClassificationConfig::default();

        let eth = make_test_iface(
            "{11111111-2222-3333-4444-555555555555}",
            Some("Ethernet"),
            Some("Intel(R) Ethernet Connection I219-V"),
            InterfaceType::Ethernet,
            Some((Ipv4Addr::new(10, 0, 0, 21), 24)),
        );
        let eth_c = classify_interface(&eth, &config);
        assert!(eth_c.is_usable_physical);
        assert!(eth_c.reason.contains("Ethernet"));

        let wifi = make_test_iface(
            "{66666666-7777-8888-9999-AAAAAAAAAAAA}",
            Some("Wi-Fi"),
            Some("Intel(R) Wi-Fi 6 AX200 160MHz"),
            InterfaceType::Wifi,
            Some((Ipv4Addr::new(192, 168, 1, 105), 24)),
        );
        let wifi_c = classify_interface(&wifi, &config);
        assert!(wifi_c.is_usable_physical);
        assert!(wifi_c.reason.contains("Wi-Fi"));
    }

    #[test]
    fn test_linux_virtual_interfaces_excluded() {
        let config = ClassificationConfig::default();

        let lo = make_test_iface(
            "lo",
            None,
            None,
            InterfaceType::Loopback,
            Some((Ipv4Addr::new(127, 0, 0, 1), 8)),
        );
        assert!(!classify_interface(&lo, &config).is_usable_physical);

        let docker = make_test_iface(
            "docker0",
            None,
            None,
            InterfaceType::Other(1),
            Some((Ipv4Addr::new(172, 17, 0, 1), 16)),
        );
        assert!(!classify_interface(&docker, &config).is_usable_physical);

        let br = make_test_iface(
            "br-404475f8d175",
            None,
            None,
            InterfaceType::Other(1),
            Some((Ipv4Addr::new(172, 22, 0, 1), 16)),
        );
        assert!(!classify_interface(&br, &config).is_usable_physical);

        let veth = make_test_iface("veth962a192", None, None, InterfaceType::Other(1), None);
        assert!(!classify_interface(&veth, &config).is_usable_physical);

        let wg = make_test_iface(
            "wg0",
            None,
            None,
            InterfaceType::Other(1),
            Some((Ipv4Addr::new(10, 9, 0, 1), 24)),
        );
        assert!(!classify_interface(&wg, &config).is_usable_physical);

        let tailscale = make_test_iface(
            "tailscale0",
            None,
            None,
            InterfaceType::Other(1),
            Some((Ipv4Addr::new(100, 64, 0, 5), 32)),
        );
        assert!(!classify_interface(&tailscale, &config).is_usable_physical);

        // eth0 in container is accepted
        let eth0 = make_test_iface(
            "eth0",
            None,
            None,
            InterfaceType::Ethernet,
            Some((Ipv4Addr::new(10, 0, 0, 21), 24)),
        );
        assert!(classify_interface(&eth0, &config).is_usable_physical);
    }

    #[test]
    fn test_auto_select_interface_by_target_subnet() {
        let config = ClassificationConfig::default();

        let eth0 = classify_interface(
            &make_test_iface(
                "eth0",
                None,
                None,
                InterfaceType::Ethernet,
                Some((Ipv4Addr::new(192, 0, 2, 21), 24)),
            ),
            &config,
        );
        let docker0 = classify_interface(
            &make_test_iface(
                "docker0",
                None,
                None,
                InterfaceType::Other(1),
                Some((Ipv4Addr::new(172, 17, 0, 1), 16)),
            ),
            &config,
        );
        let wifi = classify_interface(
            &make_test_iface(
                "wlan0",
                Some("Wi-Fi"),
                None,
                InterfaceType::Wifi,
                Some((Ipv4Addr::new(192, 168, 50, 10), 24)),
            ),
            &config,
        );

        let interfaces = vec![docker0, wifi, eth0];

        // Target receiver is at 192.0.2.106
        let selected = select_interface_for_target(&interfaces, Ipv4Addr::new(192, 0, 2, 106));
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().iface.name, "eth0");

        // Target in Wi-Fi subnet
        let selected_wifi =
            select_interface_for_target(&interfaces, Ipv4Addr::new(192, 168, 50, 99));
        assert!(selected_wifi.is_some());
        assert_eq!(selected_wifi.unwrap().iface.name, "wlan0");
    }

    #[test]
    fn test_custom_whitelist_override() {
        let mut config = ClassificationConfig::default();
        config.allowed_interfaces.push("docker0".to_string());

        let docker0 = make_test_iface(
            "docker0",
            None,
            None,
            InterfaceType::Other(1),
            Some((Ipv4Addr::new(172, 17, 0, 1), 16)),
        );
        let classified = classify_interface(&docker0, &config);
        assert!(classified.is_usable_physical);
        assert!(classified.reason.contains("whitelisted"));
    }
}
