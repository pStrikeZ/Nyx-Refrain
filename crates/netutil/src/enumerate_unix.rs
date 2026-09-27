//! Unix / Linux implementation of network interface enumeration.

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;

use crate::interface::{InterfaceInfo, InterfaceType, Ipv4Net};

/// Enumerates network interfaces on Linux / Unix systems using `getifaddrs` and `/sys/class/net`.
pub fn enumerate_interfaces_unix() -> std::io::Result<Vec<InterfaceInfo>> {
    let mut ifaddrs_ptr: *mut libc::ifaddrs = std::ptr::null_mut();
    let ret = unsafe { libc::getifaddrs(&mut ifaddrs_ptr) };
    if ret != 0 || ifaddrs_ptr.is_null() {
        return Err(std::io::Error::last_os_error());
    }

    struct IfAddrsGuard(*mut libc::ifaddrs);
    impl Drop for IfAddrsGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { libc::freeifaddrs(self.0) };
            }
        }
    }
    let _guard = IfAddrsGuard(ifaddrs_ptr);

    // Group addresses by interface name
    struct Builder {
        flags: u32,
        index: u32,
        ipv4_nets: Vec<Ipv4Net>,
    }

    let mut builders: HashMap<String, Builder> = HashMap::new();
    let mut curr = ifaddrs_ptr;

    while !curr.is_null() {
        let ifa = unsafe { &*curr };
        if !ifa.ifa_name.is_null() {
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy()
                .into_owned();

            let entry = builders.entry(name.clone()).or_insert_with(|| {
                let idx = unsafe { libc::if_nametoindex(ifa.ifa_name) };
                Builder {
                    flags: ifa.ifa_flags,
                    index: idx,
                    ipv4_nets: Vec::new(),
                }
            });

            // Check if this record is IPv4 (AF_INET)
            if !ifa.ifa_addr.is_null()
                && unsafe { (*ifa.ifa_addr).sa_family } == libc::AF_INET as u16
            {
                let sa_in = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                let ip = Ipv4Addr::from(u32::from_be(sa_in.sin_addr.s_addr));

                let prefix_len = if !ifa.ifa_netmask.is_null() {
                    let mask_in = unsafe { &*(ifa.ifa_netmask as *const libc::sockaddr_in) };
                    u32::from_be(mask_in.sin_addr.s_addr).count_ones() as u8
                } else {
                    32
                };

                entry.ipv4_nets.push(Ipv4Net::new(ip, prefix_len));
            }
        }

        curr = ifa.ifa_next;
    }

    let mut result = Vec::new();
    for (name, b) in builders {
        let is_loopback = (b.flags & libc::IFF_LOOPBACK as u32) != 0;
        let is_up = (b.flags & libc::IFF_UP as u32) != 0;

        let if_type = determine_linux_interface_type(&name, is_loopback);

        result.push(InterfaceInfo {
            name: name.clone(),
            friendly_name: Some(name),
            description: None,
            index: b.index,
            ipv4_nets: b.ipv4_nets,
            if_type,
            is_loopback,
            is_up,
        });
    }

    // Sort deterministically by index
    result.sort_by_key(|iface| iface.index);
    Ok(result)
}

fn determine_linux_interface_type(name: &str, is_loopback: bool) -> InterfaceType {
    if is_loopback || name == "lo" {
        return InterfaceType::Loopback;
    }

    let sys_path = format!("/sys/class/net/{name}");
    let p = Path::new(&sys_path);

    // Check wireless
    if p.join("wireless").exists() || p.join("phy80211").exists() || name.starts_with("wl") {
        return InterfaceType::Wifi;
    }

    // Check /sys/class/net/<name>/type
    if let Ok(type_str) = fs::read_to_string(p.join("type"))
        && let Ok(type_num) = type_str.trim().parse::<u32>()
    {
        match type_num {
            1 => return InterfaceType::Ethernet,   // ARPHRD_ETHER
            772 => return InterfaceType::Loopback, // ARPHRD_LOOPBACK
            65534 => return InterfaceType::Tunnel, // ARPHRD_NONE (TUN/TAP)
            other => return InterfaceType::Other(other),
        }
    }

    // If interface name starts with eth, en, or bridge
    if name.starts_with("eth") || name.starts_with("en") {
        InterfaceType::Ethernet
    } else {
        InterfaceType::Other(1)
    }
}
