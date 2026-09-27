//! Discovered AirPlay / RAOP device representations.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;
use unicode_normalization::UnicodeNormalization;

use raop::capabilities::DeviceCapabilities;

/// A discovered or cached AirPlay/RAOP receiver device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    /// Human-friendly display name, e.g. "Living Room" (NFC normalized).
    pub name: String,
    /// Full mDNS service instance name, e.g. "AABBCCDDEEFF@Living Room._raop._tcp.local."
    pub instance_name: String,
    /// Device IPv4 address, e.g. 192.0.2.106
    pub ip: Ipv4Addr,
    /// RTSP service port, typically 7000 for RAOP
    pub port: u16,
    /// Target host name from SRV record, e.g. "living-room.local."
    pub host: Option<String>,
    /// MAC address if extracted from instance name, e.g. "AA:BB:CC:DD:EE:FF"
    pub mac: Option<String>,
    /// Parsed TXT record capabilities (cn, et, sr, ss, ch, tp, md, vn, am, pk)
    pub capabilities: DeviceCapabilities,
    /// Raw TXT record key-value pairs
    pub txt_records: HashMap<String, String>,
    /// Unix epoch timestamp in seconds when the device was seen
    pub last_seen_epoch_secs: u64,
}

impl DiscoveredDevice {
    /// Extracts friendly name and optional MAC address from an mDNS instance string.
    ///
    /// Examples:
    /// - `"AABBCCDDEEFF@Living Room._raop._tcp.local."` -> `("Living Room", Some("AA:BB:CC:DD:EE:FF"))`
    /// - `"Living Room._airplay._tcp.local."` -> `("Living Room", None)`
    pub fn parse_instance_name(instance: &str) -> (String, Option<String>) {
        let clean = instance.trim_end_matches('.');
        let prefix = if let Some((before_service, _)) = clean.split_once("._raop.") {
            before_service
        } else if let Some((before_service, _)) = clean.split_once("._airplay.") {
            before_service
        } else {
            clean
        };

        if let Some((mac_str, raw_name)) = prefix.split_once('@') {
            let norm_name: String = raw_name.nfc().collect();
            let formatted_mac = format_mac_string(mac_str);
            (norm_name, formatted_mac)
        } else {
            let norm_name: String = prefix.nfc().collect();
            (norm_name, None)
        }
    }

    /// Formats a concise one-line summary of the device.
    pub fn summary(&self) -> String {
        let model = self.capabilities.model.as_deref().unwrap_or("Unknown");
        let et = if self.capabilities.encryption_types.is_empty() {
            "-".to_string()
        } else {
            self.capabilities
                .encryption_types
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let cn = if self.capabilities.codecs.is_empty() {
            "-".to_string()
        } else {
            self.capabilities
                .codecs
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };

        format!(
            "'{}' at {}:{} (model={}, et={}, cn={})",
            self.name, self.ip, self.port, model, et, cn
        )
    }
}

impl fmt::Display for DiscoveredDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}:{}) [{}]",
            self.name,
            self.ip,
            self.port,
            self.capabilities
                .model
                .as_deref()
                .unwrap_or("AirPlay Device")
        )
    }
}

fn format_mac_string(raw: &str) -> Option<String> {
    let clean: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if clean.len() == 12 {
        Some(
            format!(
                "{}:{}:{}:{}:{}:{}",
                &clean[0..2],
                &clean[2..4],
                &clean[4..6],
                &clean[6..8],
                &clean[8..10],
                &clean[10..12]
            )
            .to_ascii_uppercase(),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_instance_name_utf8() {
        let instance = "AABBCCDDEEFF@客厅._raop._tcp.local.";
        let (name, mac) = DiscoveredDevice::parse_instance_name(instance);
        assert_eq!(name, "客厅");
        assert_eq!(mac, Some("AA:BB:CC:DD:EE:FF".to_string()));

        let instance_airplay = "客厅._airplay._tcp.local.";
        let (name2, mac2) = DiscoveredDevice::parse_instance_name(instance_airplay);
        assert_eq!(name2, "客厅");
        assert_eq!(mac2, None);
    }
}
