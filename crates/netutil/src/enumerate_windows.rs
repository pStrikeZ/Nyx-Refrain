//! Windows implementation of network interface enumeration via GetAdaptersAddresses.

use std::net::Ipv4Addr;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

use crate::interface::{InterfaceInfo, InterfaceType, Ipv4Net};

/// Enumerates network interfaces on Windows using `GetAdaptersAddresses`.
pub fn enumerate_interfaces_windows() -> std::io::Result<Vec<InterfaceInfo>> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let family = AF_INET.0 as u32;

    let mut buf_size: u32 = 16384;
    let mut buf: Vec<u8> = vec![0u8; buf_size as usize];

    loop {
        let ret = unsafe {
            GetAdaptersAddresses(
                family,
                flags,
                None,
                Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut buf_size,
            )
        };

        if ret == NO_ERROR.0 {
            break;
        } else if ret == ERROR_BUFFER_OVERFLOW.0 {
            buf.resize(buf_size as usize, 0);
        } else if ret == ERROR_NO_DATA.0 {
            return Ok(Vec::new());
        } else {
            return Err(std::io::Error::from_raw_os_error(ret as i32));
        }
    }

    let mut result = Vec::new();
    let mut curr_ptr = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;

    while !curr_ptr.is_null() {
        let adapter = unsafe { &*curr_ptr };

        let name = unsafe { pcstr_to_string(adapter.AdapterName) };
        let friendly_name = unsafe { pcwstr_to_string(adapter.FriendlyName) };
        let description = unsafe { pcwstr_to_string(adapter.Description) };

        let index = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
        let if_type = InterfaceType::from_windows_if_type(adapter.IfType);
        let is_loopback = adapter.IfType == 24; // IF_TYPE_SOFTWARE_LOOPBACK
        let is_up = adapter.OperStatus.0 == 1; // IfOperStatusUp = 1

        let mut ipv4_nets = Vec::new();
        let mut unicast_ptr = adapter.FirstUnicastAddress;

        while !unicast_ptr.is_null() {
            let unicast = unsafe { &*unicast_ptr };
            let sockaddr_ptr = unicast.Address.lpSockaddr;

            if !sockaddr_ptr.is_null() && unsafe { (*sockaddr_ptr).sa_family } == AF_INET {
                let sin = unsafe { &*(sockaddr_ptr as *const SOCKADDR_IN) };
                let s_addr = unsafe { sin.sin_addr.S_un.S_addr };
                let ip = Ipv4Addr::from(u32::from_be(s_addr));
                let prefix_len = unicast.OnLinkPrefixLength;

                ipv4_nets.push(Ipv4Net::new(ip, prefix_len));
            }

            unicast_ptr = unicast.Next;
        }

        result.push(InterfaceInfo {
            name,
            friendly_name,
            description,
            index,
            ipv4_nets,
            if_type,
            is_loopback,
            is_up,
        });

        curr_ptr = adapter.Next;
    }

    result.sort_by_key(|iface| iface.index);
    Ok(result)
}

unsafe fn pcstr_to_string(ptr: windows::core::PSTR) -> String {
    unsafe {
        if ptr.is_null() {
            return String::new();
        }
        let mut len = 0;
        while *ptr.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr.0, len);
        String::from_utf8_lossy(slice).into_owned()
    }
}

unsafe fn pcwstr_to_string(ptr: windows::core::PWSTR) -> Option<String> {
    unsafe {
        if ptr.is_null() {
            return None;
        }
        let mut len = 0;
        while *ptr.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr.0, len);
        Some(String::from_utf16_lossy(slice))
    }
}
