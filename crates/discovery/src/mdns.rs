//! Hand-rolled DNS / mDNS protocol encoder, decoder, and record assembler.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use raop::capabilities::DeviceCapabilities;

use crate::device::DiscoveredDevice;

/// Standard mDNS IPv4 multicast address (224.0.0.251).
pub const MDNS_MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);

/// Standard mDNS UDP port (5353).
pub const MDNS_PORT: u16 = 5353;

/// Builds a DNS PTR query for the specified services.
pub fn build_mdns_ptr_query(services: &[&str]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(128);

    // 12-byte DNS Header
    // ID = 0, Flags = 0 (Query), QDCOUNT = services.len(), ANCOUNT = 0, NSCOUNT = 0, ARCOUNT = 0
    buf.extend_from_slice(&0u16.to_be_bytes()); // ID
    buf.extend_from_slice(&0u16.to_be_bytes()); // Flags: Standard Query
    buf.extend_from_slice(&(services.len() as u16).to_be_bytes()); // QDCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    for service in services {
        // Encode QNAME: length-prefixed labels
        for label in service.trim_matches('.').split('.') {
            let label_bytes = label.as_bytes();
            buf.push(label_bytes.len() as u8);
            buf.extend_from_slice(label_bytes);
        }
        buf.push(0x00); // Root label null terminator

        // QTYPE = 12 (PTR)
        buf.extend_from_slice(&12u16.to_be_bytes());
        // QCLASS = 0x8001 (IN with unicast-response QU bit set)
        buf.extend_from_slice(&0x8001u16.to_be_bytes());
    }

    buf
}

/// Helper to read a domain name from a DNS buffer supporting compression pointers (0xC0xx).
pub fn read_dns_name(buf: &[u8], mut off: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut jumped = false;
    let mut end = None;
    let mut jumps = 0;

    while off < buf.len() && jumps < 64 {
        let len = buf[off];
        if len == 0 {
            off += 1;
            break;
        }
        if (len & 0xC0) == 0xC0 {
            if off + 1 >= buf.len() {
                return None;
            }
            let ptr = (((len & 0x3F) as usize) << 8) | (buf[off + 1] as usize);
            if !jumped {
                end = Some(off + 2);
                jumped = true;
            }
            off = ptr;
            jumps += 1;
            continue;
        }
        off += 1;
        let label_len = len as usize;
        if off + label_len > buf.len() {
            return None;
        }
        let label_str = String::from_utf8_lossy(&buf[off..off + label_len]).to_string();
        labels.push(label_str);
        off += label_len;
    }

    let next_off = end.unwrap_or(off);
    let mut name = labels.join(".");
    if !name.is_empty() {
        name.push('.');
    }
    Some((name, next_off))
}

/// Raw parsed resource records from mDNS response packet(s).
#[derive(Debug, Default, Clone)]
pub struct ParsedMdnsRecords {
    /// Service PTR records: (service_name, instance_name)
    pub ptrs: Vec<(String, String)>,
    /// SRV records: (instance_name, target_host, port)
    pub srvs: Vec<(String, String, u16)>,
    /// TXT records: instance_name -> (DeviceCapabilities, raw key-value pairs)
    pub txts: HashMap<String, (DeviceCapabilities, HashMap<String, String>)>,
    /// Host A records: host_name -> IPv4
    pub hosts: HashMap<String, Ipv4Addr>,
    /// Direct instance A records if mapped
    pub instance_ips: HashMap<String, Ipv4Addr>,
}

impl ParsedMdnsRecords {
    /// Merges another parsed record set into this one.
    pub fn merge(&mut self, other: ParsedMdnsRecords) {
        self.ptrs.extend(other.ptrs);
        self.srvs.extend(other.srvs);
        for (k, v) in other.txts {
            self.txts.insert(k, v);
        }
        for (k, v) in other.hosts {
            self.hosts.insert(k, v);
        }
        for (k, v) in other.instance_ips {
            self.instance_ips.insert(k, v);
        }
    }

    /// Assembles all discovered devices from the collected records.
    pub fn assemble_devices(&self) -> Vec<DiscoveredDevice> {
        let mut devices = Vec::new();
        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 1. Walk through all PTR records
        for (_service, instance) in &self.ptrs {
            let inst_clean = instance.trim_end_matches('.');

            // Find matching SRV
            let (target_host, port) = self
                .srvs
                .iter()
                .find(|(inst, _, _)| inst.trim_end_matches('.').eq_ignore_ascii_case(inst_clean))
                .map(|(_, host, p)| (Some(host.clone()), *p))
                .unwrap_or((None, 7000));

            // Find matching IP
            let mut resolved_ip = None;
            if let Some(ref host) = target_host {
                let h_clean = host.trim_end_matches('.').to_lowercase();
                for (registered_host, ip) in &self.hosts {
                    if registered_host
                        .trim_end_matches('.')
                        .eq_ignore_ascii_case(&h_clean)
                    {
                        resolved_ip = Some(*ip);
                        break;
                    }
                }
            }

            if resolved_ip.is_none()
                && let Some(ip) = self.instance_ips.get(instance)
            {
                resolved_ip = Some(*ip);
            }

            let Some(ip) = resolved_ip else {
                continue;
            };

            // Find matching TXT
            let (capabilities, txt_records) = self
                .txts
                .iter()
                .find(|(inst, _)| inst.trim_end_matches('.').eq_ignore_ascii_case(inst_clean))
                .map(|(_, (caps, txt))| (caps.clone(), txt.clone()))
                .unwrap_or_else(|| (DeviceCapabilities::default(), HashMap::new()));

            let (name, mac) = DiscoveredDevice::parse_instance_name(instance);

            // Avoid duplicate instance names in output list
            if !devices
                .iter()
                .any(|d: &DiscoveredDevice| d.instance_name == *instance)
            {
                devices.push(DiscoveredDevice {
                    name,
                    instance_name: instance.clone(),
                    ip,
                    port,
                    host: target_host,
                    mac,
                    capabilities,
                    txt_records,
                    last_seen_epoch_secs: now_epoch,
                });
            }
        }

        devices
    }
}

/// Parses an mDNS response buffer into structured resource records.
pub fn parse_mdns_response(buf: &[u8]) -> Option<ParsedMdnsRecords> {
    if buf.len() < 12 {
        return None;
    }

    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    // Check QR bit (0x8000): must be a response
    if (flags & 0x8000) == 0 {
        return None;
    }

    let qd_count = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an_count = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let ns_count = u16::from_be_bytes([buf[8], buf[9]]) as usize;
    let ar_count = u16::from_be_bytes([buf[10], buf[11]]) as usize;

    let mut off = 12;

    // Skip questions section
    for _ in 0..qd_count {
        let (_, next_off) = read_dns_name(buf, off)?;
        off = next_off + 4; // skip QTYPE (2) and QCLASS (2)
        if off > buf.len() {
            return None;
        }
    }

    let total_rrs = an_count + ns_count + ar_count;
    let mut records = ParsedMdnsRecords::default();

    for _ in 0..total_rrs {
        if off >= buf.len() {
            break;
        }
        let (name, next_off) = read_dns_name(buf, off)?;
        off = next_off;
        if off + 10 > buf.len() {
            break;
        }

        let rtype = u16::from_be_bytes([buf[off], buf[off + 1]]);
        let rdlen = u16::from_be_bytes([buf[off + 8], buf[off + 9]]) as usize;
        off += 10;

        if off + rdlen > buf.len() {
            break;
        }

        let rdata = &buf[off..off + rdlen];
        match rtype {
            1 if rdlen == 4 => {
                // A Record (IPv4)
                let ip = Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]);
                records.hosts.insert(name.to_lowercase(), ip);
            }
            12 => {
                // PTR Record
                if let Some((target_inst, _)) = read_dns_name(buf, off) {
                    records.ptrs.push((name, target_inst));
                }
            }
            16 => {
                // TXT Record
                let caps = DeviceCapabilities::from_dns_txt_bytes(rdata);
                let raw_map = parse_raw_txt_entries(rdata);
                records.txts.insert(name, (caps, raw_map));
            }
            33 if rdlen >= 6 => {
                // SRV Record: priority (2), weight (2), port (2), target (name)
                let port = u16::from_be_bytes([rdata[4], rdata[5]]);
                if let Some((target_host, _)) = read_dns_name(buf, off + 6) {
                    records.srvs.push((name, target_host.to_lowercase(), port));
                }
            }
            _ => {}
        }

        off += rdlen;
    }

    Some(records)
}

fn parse_raw_txt_entries(bytes: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut off = 0;
    while off < bytes.len() {
        let len = bytes[off] as usize;
        off += 1;
        if off + len > bytes.len() {
            break;
        }
        let chunk = &bytes[off..off + len];
        off += len;
        if let Ok(s) = std::str::from_utf8(chunk) {
            if let Some((k, v)) = s.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            } else if !s.trim().is_empty() {
                map.insert(s.trim().to_string(), String::new());
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_mdns_query() {
        let query = build_mdns_ptr_query(&["_raop._tcp.local", "_airplay._tcp.local"]);
        assert!(query.len() > 12);
        // Header assertions: QDCOUNT = 2
        assert_eq!(query[4..6], 2u16.to_be_bytes());
    }

    #[test]
    fn test_assemble_devices() {
        let mut records = ParsedMdnsRecords::default();

        records.ptrs.push((
            "_raop._tcp.local.".to_string(),
            "AABBCCDDEEFF@Living Room._raop._tcp.local.".to_string(),
        ));
        records.srvs.push((
            "AABBCCDDEEFF@Living Room._raop._tcp.local.".to_string(),
            "living-room.local.".to_string(),
            7000,
        ));
        records.hosts.insert(
            "living-room.local.".to_string(),
            Ipv4Addr::new(192, 0, 2, 106),
        );

        // TXT data (lengths: cn=0,1,2,3 -> 10, et=0,3,5 -> 8, am=AudioAccessory5,1 -> 20)
        let txt_bytes = b"\x0acn=0,1,2,3\x08et=0,3,5\x14am=AudioAccessory5,1";
        let caps = DeviceCapabilities::from_dns_txt_bytes(txt_bytes);
        let raw = parse_raw_txt_entries(txt_bytes);
        records.txts.insert(
            "AABBCCDDEEFF@Living Room._raop._tcp.local.".to_string(),
            (caps, raw),
        );

        let devices = records.assemble_devices();
        assert_eq!(devices.len(), 1);

        let dev = &devices[0];
        assert_eq!(dev.name, "Living Room");
        assert_eq!(dev.ip, Ipv4Addr::new(192, 0, 2, 106));
        assert_eq!(dev.port, 7000);
        assert_eq!(dev.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(dev.capabilities.model.as_deref(), Some("AudioAccessory5,1"));
        assert_eq!(dev.capabilities.codecs, vec![0, 1, 2, 3]);
        assert_eq!(dev.capabilities.encryption_types, vec![0, 3, 5]);
    }
}
